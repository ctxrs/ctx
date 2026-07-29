use std::path::Path;

use ctx_history_store::Store;

use crate::provider::providers::continue_cli::import_continue_cli_nativepath;
use crate::provider::providers::deepagents::import_deepagents_nativepath;
use crate::provider::providers::firebender::firebender_source_root;
use crate::provider::providers::firebender::import_firebender_nativepath;
use crate::provider::providers::forgecode::import_forgecode_nativepath;
use crate::provider::providers::kiro::import_kiro_nativepath;
use crate::provider::providers::nanoclaw::import_nanoclaw_nativepath;
use crate::provider::providers::opencode::{
    import_opencode_nativepath, KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT,
    OPENCODE_SQLITE_DIALECT,
};
use crate::provider::providers::openhands::import_openhands_nativepath;
use crate::provider::providers::shelley::import_shelley_nativepath;
use crate::{
    ContinueCliImportOptions, DeepAgentsSqliteImportOptions,
    FirebenderSqliteImportOptions, ForgeCodeSqliteImportOptions, KiloSqliteImportOptions,
    KiroSqliteImportOptions, MiMoCodeSqliteImportOptions, NanoClawImportOptions,
    OpenCodeSqliteImportOptions, OpenHandsImportOptions, ProviderAdapterContext,
    ProviderImportOptions, ProviderImportSummary, Result, ShelleySqliteImportOptions,
};

pub fn import_firebender_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: FirebenderSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let source_root = firebender_source_root(path)?;
    import_firebender_nativepath(
        path,
        store,
        ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            source_root: Some(source_root),
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

pub fn import_opencode_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenCodeSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_opencode_nativepath(
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
        &OPENCODE_SQLITE_DIALECT,
    )
}

pub fn import_kilo_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KiloSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_opencode_nativepath(
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
        &KILO_SQLITE_DIALECT,
    )
}

pub fn import_forgecode_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ForgeCodeSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_forgecode_nativepath(
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

pub fn import_deepagents_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: DeepAgentsSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_deepagents_nativepath(
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

pub fn import_nanoclaw_project(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: NanoClawImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_nanoclaw_nativepath(
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

pub fn import_kiro_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: KiroSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_kiro_nativepath(
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

pub fn import_shelley_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ShelleySqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_shelley_nativepath(
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

pub fn import_continue_cli_sessions(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ContinueCliImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_continue_cli_nativepath(
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

pub fn import_openhands_file_events(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: OpenHandsImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_openhands_nativepath(
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

pub fn import_mimocode_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: MiMoCodeSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    import_opencode_nativepath(
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
        &MIMOCODE_SQLITE_DIALECT,
    )
}
