#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Copy, Default)]
pub struct TabnineCliJsonlAdapter;

impl ProviderCaptureAdapter for TabnineCliJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Tabnine
    }

    fn source_format(&self) -> &str {
        TABNINE_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Tabnine,
            TABNINE_CLI_SOURCE_FORMAT,
        )
    }
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

pub(crate) const TABNINE_CLI_SOURCE_FORMAT: &str = "tabnine_cli_chat_recording_jsonl";
