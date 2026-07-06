use std::path::Path;

use ctx_history_core::CaptureProvider;

use crate::provider::adapter::{
    AntigravityCliJsonlAdapter, CopilotCliSessionEventsAdapter, CursorAgentTranscriptJsonlAdapter,
    FactoryAiDroidJsonlAdapter, GeminiCliJsonlAdapter, QwenCodeJsonlAdapter,
    TabnineCliJsonlAdapter, WindsurfCascadeHookTranscriptJsonlAdapter,
};
use crate::provider::providers::native_jsonl::normalize_jsonl_tree;
use crate::{
    ProviderAdapterContext, ProviderCaptureAdapter, ProviderNormalizationResult, Result,
    ANTIGRAVITY_CLI_SOURCE_FORMAT, COPILOT_CLI_SOURCE_FORMAT,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, FACTORY_DROID_SOURCE_FORMAT, GEMINI_CLI_SOURCE_FORMAT,
    QWEN_CODE_SOURCE_FORMAT, TABNINE_CLI_SOURCE_FORMAT,
    WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
};

macro_rules! jsonl_tree_adapter {
    ($adapter:ty, $provider:path, $format:expr) => {
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
                normalize_jsonl_tree(path, context, $provider, $format)
            }
        }
    };
}

jsonl_tree_adapter!(
    AntigravityCliJsonlAdapter,
    CaptureProvider::Antigravity,
    ANTIGRAVITY_CLI_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    GeminiCliJsonlAdapter,
    CaptureProvider::Gemini,
    GEMINI_CLI_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    TabnineCliJsonlAdapter,
    CaptureProvider::Tabnine,
    TABNINE_CLI_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    CursorAgentTranscriptJsonlAdapter,
    CaptureProvider::Cursor,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    WindsurfCascadeHookTranscriptJsonlAdapter,
    CaptureProvider::Windsurf,
    WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    FactoryAiDroidJsonlAdapter,
    CaptureProvider::FactoryAiDroid,
    FACTORY_DROID_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    CopilotCliSessionEventsAdapter,
    CaptureProvider::CopilotCli,
    COPILOT_CLI_SOURCE_FORMAT
);
jsonl_tree_adapter!(
    QwenCodeJsonlAdapter,
    CaptureProvider::QwenCode,
    QWEN_CODE_SOURCE_FORMAT
);
