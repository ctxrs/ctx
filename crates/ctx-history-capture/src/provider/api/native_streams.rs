use std::path::Path;

use ctx_history_store::Store;

use crate::provider::providers::kimi::import_kimi_nativepath_tree;
use crate::provider::providers::lingma::import_lingma_nativepath;
use crate::provider::providers::mistral_vibe::import_mistral_vibe_nativepath;
use crate::provider::providers::mux::import_mux_native_path;
use crate::provider::providers::native_jsonl::{
    import_antigravity_nativepath_tree, import_copilot_nativepath_tree,
    import_cursor_nativepath_tree, import_factory_ai_droid_nativepath_tree,
    import_gemini_nativepath_tree, import_qoder_nativepath_tree, import_qwen_code_nativepath_tree,
    import_tabnine_nativepath_tree, import_windsurf_nativepath_tree, NativePathJsonlTreeImport,
};
use crate::provider::providers::rovodev::import_rovodev_native_path;
use crate::provider::providers::warp::import_warp_nativepath;
use crate::provider::providers::zed::import_zed_nativepath;
use crate::{
    AntigravityCliImportOptions, CopilotCliImportOptions, CursorNativeImportOptions,
    FactoryAiDroidImportOptions, GeminiCliImportOptions, KimiCodeCliImportOptions,
    LingmaSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportSummary, QoderImportOptions, QwenCodeImportOptions,
    Result, RovoDevImportOptions, TabnineCliImportOptions, WarpSqliteImportOptions,
    WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions,
};

pub fn import_antigravity_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AntigravityCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_antigravity_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
    )
}

pub fn import_gemini_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GeminiCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_gemini_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
    )
}

pub fn import_tabnine_cli_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: TabnineCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_tabnine_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
    )
}

pub fn import_cursor_native_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CursorNativeImportOptions,
) -> Result<ProviderImportSummary> {
    import_cursor_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
    )
}

pub fn import_windsurf_cascade_hook_transcripts(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WindsurfCascadeHookImportOptions,
) -> Result<ProviderImportSummary> {
    import_windsurf_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
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
    import_warp_nativepath(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
        },
    )
}

pub fn import_qoder_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QoderImportOptions,
) -> Result<ProviderImportSummary> {
    import_qoder_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
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
    import_zed_nativepath(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
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
    import_lingma_nativepath(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
        },
    )
}

pub fn import_factory_ai_droid_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: FactoryAiDroidImportOptions,
) -> Result<ProviderImportSummary> {
    import_factory_ai_droid_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
    )
}

pub fn import_copilot_cli_session_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CopilotCliImportOptions,
) -> Result<ProviderImportSummary> {
    import_copilot_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
    )
}

pub fn import_qwen_code_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: QwenCodeImportOptions,
) -> Result<ProviderImportSummary> {
    import_qwen_code_nativepath_tree(
        store,
        NativePathJsonlTreeImport {
            path: path.as_ref(),
            machine_id: options.machine_id,
            source_path: options.source_path,
            source_root: None,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token,
            import_profile: options.import_profile,
        },
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
    import_kimi_nativepath_tree(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
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
    import_rovodev_native_path(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
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
    import_mistral_vibe_nativepath(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
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
    import_mux_native_path(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: None,
            imported_at: options.imported_at,
        },
        ProviderImportOptions {
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            inventory_observation_token: options.inventory_observation_token.clone(),
            import_profile: options.import_profile.clone(),
        },
    )
}
