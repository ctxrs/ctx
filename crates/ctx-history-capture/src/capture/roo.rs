#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct RooTaskJsonImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for RooTaskJsonImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RooTaskJsonAdapter;

impl ProviderCaptureAdapter for RooTaskJsonAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::RooCode
    }

    fn source_format(&self) -> &str {
        ROO_TASK_JSON_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_task_json_history(path, context, task_json_provider(CaptureProvider::RooCode))
    }
}

pub fn import_roo_task_json_history(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: RooTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = RooTaskJsonAdapter.normalize_path(
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

pub(crate) const ROO_TASK_JSON_SOURCE_FORMAT: &str = "roo_task_directory_json";

pub(crate) fn task_json_provider(provider: CaptureProvider) -> TaskJsonProviderSpec {
    match provider {
        CaptureProvider::RooCode => TaskJsonProviderSpec {
            provider,
            source_format: ROO_TASK_JSON_SOURCE_FORMAT,
            display_name: "Roo Code",
            api_file: "api_conversation_history.json",
            ui_file: "ui_messages.json",
            metadata_file: "task_metadata.json",
            history_item_file: Some("history_item.json"),
            index_file: Some("_index.json"),
            fallback_api_file: Some("claude_messages.json"),
        },
        _ => TaskJsonProviderSpec {
            provider: CaptureProvider::Cline,
            source_format: CLINE_TASK_JSON_SOURCE_FORMAT,
            display_name: "Cline",
            api_file: "api_conversation_history.json",
            ui_file: "ui_messages.json",
            metadata_file: "task_metadata.json",
            history_item_file: None,
            index_file: None,
            fallback_api_file: None,
        },
    }
}
