use std::path::Path;

use ctx_history_store::Store;

use crate::provider::adapter::{
    AntigravityCliJsonlAdapter, CopilotCliSessionEventsAdapter, CursorAgentTranscriptJsonlAdapter,
    FactoryAiDroidJsonlAdapter, GeminiCliJsonlAdapter, KimiCodeCliWireJsonlAdapter,
    LingmaSqliteAdapter, MistralVibeJsonlAdapter, MuxJsonlAdapter, ProviderCaptureAdapter,
    QoderJsonlAdapter, QwenCodeJsonlAdapter, RovoDevSessionJsonAdapter, TabnineCliJsonlAdapter,
    WindsurfCascadeHookTranscriptJsonlAdapter, ZedThreadsSqliteAdapter,
};
use crate::provider::importer::{
    import_native_jsonl_tree, import_normalized_provider_captures, NativeJsonlTreeImport,
};
use crate::provider::providers::warp::normalize_warp_sqlite;
use crate::{
    AntigravityCliImportOptions, CodexEventImportMode, CodexToolOutputMode,
    CopilotCliImportOptions, CursorNativeImportOptions, FactoryAiDroidImportOptions,
    GeminiCliImportOptions, KimiCodeCliImportOptions, LingmaSqliteImportOptions,
    MistralVibeImportOptions, MuxImportOptions, NormalizedProviderImportOptions,
    ProviderAdapterContext, ProviderImportSummary, QoderImportOptions, QwenCodeImportOptions,
    Result, RovoDevImportOptions, TabnineCliImportOptions, WarpSqliteImportOptions,
    WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};

pub fn import_antigravity_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AntigravityCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        AntigravityCliJsonlAdapter,
    )
}

pub fn import_gemini_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GeminiCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        GeminiCliJsonlAdapter,
    )
}

pub fn import_tabnine_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: TabnineCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        TabnineCliJsonlAdapter,
    )
}

pub fn import_cursor_native_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CursorNativeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        CursorAgentTranscriptJsonlAdapter,
    )
}

pub fn import_windsurf_cascade_hook_transcripts(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WindsurfCascadeHookImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        WindsurfCascadeHookTranscriptJsonlAdapter,
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
    let normalization = normalize_warp_sqlite(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        },
    )
}

pub fn import_qoder_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QoderImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        QoderJsonlAdapter,
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
    let normalization = ZedThreadsSqliteAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
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
    let normalization = LingmaSqliteAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_factory_ai_droid_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: FactoryAiDroidImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        FactoryAiDroidJsonlAdapter,
    )
}

pub fn import_copilot_cli_session_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CopilotCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        CopilotCliSessionEventsAdapter,
    )
}

pub fn import_qwen_code_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QwenCodeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        QwenCodeJsonlAdapter,
    )
}

pub fn import_kimi_code_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KimiCodeCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        KimiCodeCliWireJsonlAdapter,
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
    let normalization = RovoDevSessionJsonAdapter.normalize_path(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_mistral_vibe_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MistralVibeImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        MistralVibeJsonlAdapter,
    )
}

pub fn import_mux_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MuxImportOptions,
) -> Result<ProviderImportSummary> {
    import_native_jsonl_tree(
        store,
        NativeJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
        },
        MuxJsonlAdapter,
    )
}
