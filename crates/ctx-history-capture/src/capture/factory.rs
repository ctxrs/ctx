#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct FactoryAiDroidImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for FactoryAiDroidImportOptions {
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
pub struct FactoryAiDroidJsonlAdapter;

impl ProviderCaptureAdapter for FactoryAiDroidJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::FactoryAiDroid
    }

    fn source_format(&self) -> &str {
        FACTORY_DROID_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::FactoryAiDroid,
            FACTORY_DROID_SOURCE_FORMAT,
        )
    }
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

pub(crate) const FACTORY_DROID_SOURCE_FORMAT: &str = "factory_ai_droid_sessions_jsonl";

pub(crate) fn native_jsonl_header_session_id(
    provider: CaptureProvider,
    value: &Value,
) -> Option<String> {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            value.get("sessionId").and_then(Value::as_str)
        }
        CaptureProvider::FactoryAiDroid => (value.get("type").and_then(Value::as_str)
            == Some("session_start"))
        .then(|| value.get("sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::CopilotCli => (value.get("type").and_then(Value::as_str)
            == Some("session.start"))
        .then(|| value.pointer("/data/sessionId").and_then(Value::as_str))
        .flatten(),
        CaptureProvider::QwenCode => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::Qoder => value.get("sessionId").and_then(Value::as_str),
        CaptureProvider::Cursor => (value.get("role").is_some()
            || value.get("event").is_some()
            || value.get("message").is_some())
        .then_some("cursor-path-session"),
        _ => None,
    }
    .filter(|id| !id.trim().is_empty())
    .map(str::to_owned)
}
