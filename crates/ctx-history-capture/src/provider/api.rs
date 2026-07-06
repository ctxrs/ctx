use std::{io::BufRead, path::Path};

use ctx_history_store::Store;

use crate::provider::adapter::{ProviderCaptureAdapter, ProviderFixtureJsonlAdapter};
use crate::provider::custom_history_jsonl::{
    import_custom_history_edges, import_custom_history_source_cursors,
    normalize_custom_history_jsonl_v1, normalize_custom_history_jsonl_v1_reader,
};
use crate::provider::importer::import_normalized_provider_captures;
use crate::{
    CodexEventImportMode, CodexToolOutputMode, CustomHistoryJsonlV1ImportOptions,
    NormalizedProviderImportOptions, ProviderAdapterContext, ProviderFixtureImportOptions,
    ProviderImportSummary, Result,
};

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
    import_astrbot_sqlite, import_continue_cli_sessions, import_deepagents_sqlite,
    import_firebender_sqlite, import_forgecode_sqlite, import_kilo_sqlite, import_kiro_sqlite,
    import_nanoclaw_project, import_opencode_sqlite, import_openhands_file_events,
    import_shelley_sqlite,
};

pub fn import_provider_fixture_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ProviderFixtureImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ProviderFixtureJsonlAdapter {
        expected_provider: options.expected_provider,
        source_format: options.source_format.clone(),
        fidelity: options.fidelity,
    }
    .normalize_path(
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

pub fn import_custom_history_jsonl_v1(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = normalize_custom_history_jsonl_v1(
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
    if normalization.provider.summary.failed > 0 && !options.allow_partial_failures {
        return Ok(normalization.provider.summary);
    }

    let mut summary = import_normalized_provider_captures(
        store,
        normalization.provider,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )?;
    import_custom_history_edges(
        store,
        &normalization.edges,
        options.history_record_id,
        options.allow_partial_failures,
        &mut summary,
    )?;
    import_custom_history_source_cursors(store, &normalization.source_cursors)?;
    Ok(summary)
}

pub fn import_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let normalization = normalize_custom_history_jsonl_v1_reader(
        reader,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    if normalization.provider.summary.failed > 0 && !options.allow_partial_failures {
        return Ok(normalization.provider.summary);
    }

    let mut summary = import_normalized_provider_captures(
        store,
        normalization.provider,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )?;
    import_custom_history_edges(
        store,
        &normalization.edges,
        options.history_record_id,
        options.allow_partial_failures,
        &mut summary,
    )?;
    import_custom_history_source_cursors(store, &normalization.source_cursors)?;
    Ok(summary)
}

pub fn validate_custom_history_jsonl_v1(path: impl AsRef<Path>) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let normalization = normalize_custom_history_jsonl_v1(
        path,
        &ProviderAdapterContext {
            source_path: Some(path.to_path_buf()),
            ..ProviderAdapterContext::default()
        },
    )?;
    Ok(normalization.provider.summary)
}

pub fn validate_custom_history_jsonl_v1_reader(
    reader: impl BufRead,
) -> Result<ProviderImportSummary> {
    let normalization =
        normalize_custom_history_jsonl_v1_reader(reader, &ProviderAdapterContext::default())?;
    Ok(normalization.provider.summary)
}
