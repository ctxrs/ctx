#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct QoderImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for QoderImportOptions {
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
pub struct QoderJsonlAdapter;

impl ProviderCaptureAdapter for QoderJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Qoder
    }

    fn source_format(&self) -> &str {
        QODER_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(path, context, CaptureProvider::Qoder, QODER_SOURCE_FORMAT)
    }
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

pub(crate) const QODER_SOURCE_FORMAT: &str = "qoder_transcript_jsonl";
