use std::path::Path;

use ctx_history_store::Store;

use crate::provider::providers::auggie::import_auggie_sessions_nativepath;
use crate::provider::providers::claude::import_claude_nativepath_projects;
use crate::provider::providers::codebuddy::import_codebuddy_nativepath;
use crate::provider::providers::crush::import_crush_nativepath;
use crate::provider::providers::goose::import_goose_nativepath;
use crate::provider::providers::hermes::import_hermes_nativepath;
use crate::provider::providers::junie::import_junie_nativepath;
use crate::provider::providers::openclaw::import_openclaw_nativepath_tree;
use crate::provider::providers::pi::import_pi_nativepath_history;
use crate::provider::providers::task_json::cline_nativepath::{
    import_cline_nativepath_history, import_roo_nativepath_history,
};
use crate::provider::providers::trae::import_trae_nativepath;
use crate::{
    AuggieImportOptions, ClaudeProjectsImportOptions, ClineTaskJsonImportOptions,
    CodeBuddyImportOptions, CrushSqliteImportOptions, GooseSessionsSqliteImportOptions,
    HermesSqliteImportOptions, JunieImportOptions, OpenClawImportOptions, PiSessionImportOptions,
    ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
    RooTaskJsonImportOptions, TraeImportOptions,
};

pub fn import_pi_session_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: PiSessionImportOptions,
) -> Result<ProviderImportSummary> {
    import_pi_nativepath_history(path.as_ref(), store, options)
}

pub fn import_claude_projects_jsonl_tree(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ClaudeProjectsImportOptions,
) -> Result<ProviderImportSummary> {
    import_claude_nativepath_projects(path.as_ref(), store, options)
}

pub fn import_cline_task_json_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ClineTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    import_cline_nativepath_history(path.as_ref(), store, options)
}

pub fn import_roo_task_json_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: RooTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    import_roo_nativepath_history(path.as_ref(), store, options)
}

pub fn import_codebuddy_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodeBuddyImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_codebuddy_nativepath(
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

pub fn import_trae_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: TraeImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_trae_nativepath(
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

pub fn import_crush_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CrushSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_crush_nativepath(
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

pub fn import_goose_sessions_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: GooseSessionsSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_goose_nativepath(
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

pub fn import_openclaw_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenClawImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_openclaw_nativepath_tree(
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

pub fn import_hermes_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: HermesSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_hermes_nativepath(
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

pub fn import_auggie_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: AuggieImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_auggie_sessions_nativepath(
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

pub fn import_junie_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: JunieImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_junie_nativepath(
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
