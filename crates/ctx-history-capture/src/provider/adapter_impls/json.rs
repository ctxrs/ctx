use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::common::io::collect_jsonl_paths;
use crate::provider::adapter::{
    AuggieSessionJsonAdapter, ClaudeProjectsJsonlAdapter, ClineTaskJsonAdapter,
    CodeBuddyHistoryJsonAdapter, ContinueCliSessionsAdapter, JunieSessionEventsAdapter,
    KimiCodeCliWireJsonlAdapter, MistralVibeJsonlAdapter, MuxJsonlAdapter, NanoClawProjectAdapter,
    OpenClawJsonlAdapter, OpenHandsFileEventsAdapter, QoderJsonlAdapter, RooTaskJsonAdapter,
    RovoDevSessionJsonAdapter,
};
use crate::provider::providers::{
    auggie::normalize_auggie_sessions,
    claude::normalize_claude_projects_jsonl_file,
    codebuddy::normalize_codebuddy_history,
    continue_cli::normalize_continue_cli_sessions,
    junie::normalize_junie_session_events,
    kimi::normalize_kimi_code_cli_history,
    mistral_vibe::normalize_mistral_vibe_sessions,
    mux::normalize_mux_sessions,
    nanoclaw::normalize_nanoclaw_project,
    openclaw::normalize_openclaw_history,
    openhands::normalize_openhands_file_events,
    rovodev::normalize_rovodev_sessions,
    task_json::{normalize_task_json_history, task_json_provider},
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderCaptureAdapter, ProviderNormalizationResult,
    Result, AUGGIE_SESSION_JSON_SOURCE_FORMAT, CLAUDE_PROJECTS_SOURCE_FORMAT,
    CLINE_TASK_JSON_SOURCE_FORMAT, CODEBUDDY_SOURCE_FORMAT, CONTINUE_CLI_SOURCE_FORMAT,
    JUNIE_SESSION_EVENTS_SOURCE_FORMAT, KIMI_CODE_CLI_SOURCE_FORMAT, MISTRAL_VIBE_SOURCE_FORMAT,
    MUX_SOURCE_FORMAT, NANOCLAW_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT,
    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT, QODER_SOURCE_FORMAT, ROO_TASK_JSON_SOURCE_FORMAT,
    ROVODEV_SOURCE_FORMAT,
};

impl ProviderCaptureAdapter for ClaudeProjectsJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Claude
    }

    fn source_format(&self) -> &str {
        CLAUDE_PROJECTS_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        let mut paths = Vec::new();
        collect_jsonl_paths(path, &mut paths)?;
        paths.sort();
        if paths.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "no Claude Code project JSONL transcripts found",
            });
        }

        let mut merged = ProviderNormalizationResult::default();
        for path in paths {
            let mut result = normalize_claude_projects_jsonl_file(&path, context)?;
            merged.summary.merge(result.summary);
            merged.captures.append(&mut result.captures);
            merged.files_touched.append(&mut result.files_touched);
        }
        Ok(merged)
    }
}

macro_rules! json_adapter {
    ($adapter:ty, $provider:path, $format:expr, $normalize:expr) => {
        impl ProviderCaptureAdapter for $adapter {
            fn provider(&self) -> CaptureProvider {
                $provider
            }

            fn source_format(&self) -> &str {
                $format
            }

            fn normalize_path(
                &self,
                path: &Path,
                context: &ProviderAdapterContext,
            ) -> Result<ProviderNormalizationResult> {
                $normalize(path, context)
            }
        }
    };
}

json_adapter!(
    CodeBuddyHistoryJsonAdapter,
    CaptureProvider::CodeBuddy,
    CODEBUDDY_SOURCE_FORMAT,
    normalize_codebuddy_history
);
json_adapter!(
    AuggieSessionJsonAdapter,
    CaptureProvider::Auggie,
    AUGGIE_SESSION_JSON_SOURCE_FORMAT,
    normalize_auggie_sessions
);
json_adapter!(
    JunieSessionEventsAdapter,
    CaptureProvider::Junie,
    JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
    normalize_junie_session_events
);
json_adapter!(
    OpenClawJsonlAdapter,
    CaptureProvider::OpenClaw,
    OPENCLAW_SOURCE_FORMAT,
    normalize_openclaw_history
);
json_adapter!(
    NanoClawProjectAdapter,
    CaptureProvider::NanoClaw,
    NANOCLAW_SOURCE_FORMAT,
    normalize_nanoclaw_project
);
json_adapter!(
    ContinueCliSessionsAdapter,
    CaptureProvider::Continue,
    CONTINUE_CLI_SOURCE_FORMAT,
    normalize_continue_cli_sessions
);
json_adapter!(
    OpenHandsFileEventsAdapter,
    CaptureProvider::OpenHands,
    OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
    normalize_openhands_file_events
);
json_adapter!(
    KimiCodeCliWireJsonlAdapter,
    CaptureProvider::KimiCodeCli,
    KIMI_CODE_CLI_SOURCE_FORMAT,
    normalize_kimi_code_cli_history
);
json_adapter!(
    RovoDevSessionJsonAdapter,
    CaptureProvider::RovoDev,
    ROVODEV_SOURCE_FORMAT,
    normalize_rovodev_sessions
);
json_adapter!(
    MistralVibeJsonlAdapter,
    CaptureProvider::MistralVibe,
    MISTRAL_VIBE_SOURCE_FORMAT,
    normalize_mistral_vibe_sessions
);
json_adapter!(
    MuxJsonlAdapter,
    CaptureProvider::Mux,
    MUX_SOURCE_FORMAT,
    normalize_mux_sessions
);

impl ProviderCaptureAdapter for ClineTaskJsonAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cline
    }

    fn source_format(&self) -> &str {
        CLINE_TASK_JSON_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_task_json_history(path, context, task_json_provider(CaptureProvider::Cline))
    }
}

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
        crate::provider::providers::native_jsonl::normalize_jsonl_tree(
            path,
            context,
            CaptureProvider::Qoder,
            QODER_SOURCE_FORMAT,
        )
    }
}
