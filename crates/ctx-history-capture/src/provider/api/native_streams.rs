use std::path::Path;

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::provider::providers::kimi::import_kimi_wire_jsonl_tree_batched;
use crate::provider::providers::lingma::import_lingma_sqlite_batched;
use crate::provider::providers::mistral_vibe::import_mistral_vibe_sessions_batched;
use crate::provider::providers::mux::import_mux_sessions_batched;
use crate::provider::providers::native_jsonl::{
    import_bounded_native_jsonl_tree, NativeJsonlTreeImport,
};
use crate::provider::providers::rovodev::import_rovodev_sessions_batched;
use crate::provider::providers::warp::import_warp_sqlite_batched;
use crate::provider::providers::zed::import_zed_threads_sqlite_batched;
use crate::{
    AntigravityCliImportOptions, CopilotCliImportOptions, CursorNativeImportOptions,
    FactoryAiDroidImportOptions, GeminiCliImportOptions, KimiCodeCliImportOptions,
    LingmaSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions,
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    QoderImportOptions, QwenCodeImportOptions, Result, RovoDevImportOptions,
    TabnineCliImportOptions, WarpSqliteImportOptions, WindsurfCascadeHookImportOptions,
    ZedThreadsSqliteImportOptions, ANTIGRAVITY_CLI_SOURCE_FORMAT, COPILOT_CLI_SOURCE_FORMAT,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, FACTORY_DROID_SOURCE_FORMAT, GEMINI_CLI_SOURCE_FORMAT,
    QODER_SOURCE_FORMAT, QWEN_CODE_SOURCE_FORMAT, TABNINE_CLI_SOURCE_FORMAT,
    WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
};

pub fn import_antigravity_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AntigravityCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::Antigravity,
        ANTIGRAVITY_CLI_SOURCE_FORMAT,
    )
}

pub fn import_gemini_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GeminiCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
    )
}

pub fn import_tabnine_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: TabnineCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::Tabnine,
        TABNINE_CLI_SOURCE_FORMAT,
    )
}

pub fn import_cursor_native_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CursorNativeImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::Cursor,
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
    )
}

pub fn import_windsurf_cascade_hook_transcripts(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WindsurfCascadeHookImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::Windsurf,
        WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
    )
}

pub fn import_warp_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WarpSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_warp_sqlite_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

pub fn import_qoder_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QoderImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::Qoder,
        QODER_SOURCE_FORMAT,
    )
}

pub fn import_zed_threads_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ZedThreadsSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_zed_threads_sqlite_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

pub fn import_lingma_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: LingmaSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_lingma_sqlite_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

pub fn import_factory_ai_droid_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: FactoryAiDroidImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::FactoryAiDroid,
        FACTORY_DROID_SOURCE_FORMAT,
    )
}

pub fn import_copilot_cli_session_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CopilotCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
    )
}

pub fn import_qwen_code_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QwenCodeImportOptions,
) -> Result<ProviderImportSummary> {
    import_bounded_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
        },
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
    )
}

pub fn import_kimi_code_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KimiCodeCliImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_kimi_wire_jsonl_tree_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

pub fn import_rovodev_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: RovoDevImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_rovodev_sessions_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

pub fn import_mistral_vibe_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MistralVibeImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_mistral_vibe_sessions_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}

pub fn import_mux_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MuxImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_mux_sessions_batched(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
        },
    )
}
