#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct CopilotCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for CopilotCliImportOptions {
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
pub struct CopilotCliSessionEventsAdapter;

impl ProviderCaptureAdapter for CopilotCliSessionEventsAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::CopilotCli
    }

    fn source_format(&self) -> &str {
        COPILOT_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::CopilotCli,
            COPILOT_CLI_SOURCE_FORMAT,
        )
    }
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

pub(crate) const COPILOT_CLI_SOURCE_FORMAT: &str = "copilot_cli_session_events_jsonl";

pub(crate) fn native_jsonl_session_status(
    provider: CaptureProvider,
    header: &Value,
) -> SessionStatus {
    if provider == CaptureProvider::CopilotCli
        && header.get("type").and_then(Value::as_str) == Some("abort")
    {
        SessionStatus::Interrupted
    } else {
        SessionStatus::Imported
    }
}
