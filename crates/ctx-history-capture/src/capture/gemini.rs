#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct GeminiCliImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for GeminiCliImportOptions {
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

pub type TabnineCliImportOptions = GeminiCliImportOptions;

#[derive(Debug, Clone, Copy, Default)]
pub struct GeminiCliJsonlAdapter;

impl ProviderCaptureAdapter for GeminiCliJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Gemini
    }

    fn source_format(&self) -> &str {
        GEMINI_CLI_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
        )
    }
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

pub(crate) const GEMINI_CLI_SOURCE_FORMAT: &str = "gemini_cli_chat_recording_jsonl";

pub(crate) fn native_jsonl_header_cwd(provider: CaptureProvider, value: &Value) -> Option<String> {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => value
            .get("directories")
            .and_then(Value::as_array)
            .and_then(|dirs| dirs.first())
            .and_then(Value::as_str),
        CaptureProvider::FactoryAiDroid => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::CopilotCli => value.pointer("/data/context/cwd").and_then(Value::as_str),
        CaptureProvider::QwenCode => value.get("cwd").and_then(Value::as_str),
        CaptureProvider::Qoder => value.get("cwd").and_then(Value::as_str),
        _ => None,
    }
    .filter(|cwd| !cwd.trim().is_empty())
    .map(str::to_owned)
}

pub(crate) fn native_jsonl_path_session(
    provider: CaptureProvider,
    path: &Path,
    header: &Value,
    native_session_id: &str,
) -> (String, Option<String>, Option<String>, AgentType) {
    match provider {
        CaptureProvider::Gemini | CaptureProvider::Tabnine => {
            let parent = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str());
            if parent.is_some_and(|name| name != "chats") {
                return (
                    native_session_id.to_owned(),
                    parent.map(str::to_owned),
                    None,
                    AgentType::Subagent,
                );
            }
            (native_session_id.to_owned(), None, None, AgentType::Primary)
        }
        CaptureProvider::FactoryAiDroid => {
            let parent = header
                .get("parent")
                .or_else(|| header.get("callingSessionId"))
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned);
            let agent_type = if parent.is_some()
                || header.get("decompSessionType").and_then(Value::as_str) == Some("worker")
            {
                AgentType::Subagent
            } else {
                AgentType::Primary
            };
            (
                native_session_id.to_owned(),
                parent,
                header
                    .get("decompMissionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                agent_type,
            )
        }
        CaptureProvider::Cursor => {
            let session = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or(native_session_id)
                .to_owned();
            (session, None, None, AgentType::Primary)
        }
        _ => (native_session_id.to_owned(), None, None, AgentType::Primary),
    }
}

pub(crate) fn gemini_tool_calls_have_result(value: &Value) -> bool {
    value
        .get("toolCalls")
        .and_then(Value::as_array)
        .map(|calls| calls.iter().any(|call| call.get("result").is_some()))
        .unwrap_or(false)
}
