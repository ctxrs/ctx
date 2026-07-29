use std::path::Path;

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::{CaptureError, Result};

use super::normalization::native_jsonl_header_session_id;

fn provider_jsonl_path_is_native(provider: CaptureProvider, path: &Path) -> bool {
    match provider {
        CaptureProvider::Antigravity => {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }
        CaptureProvider::Gemini | CaptureProvider::Tabnine => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::Windsurf => path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
        CaptureProvider::Qoder => {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "transcript")
        }
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        _ => true,
    }
}

pub(super) fn native_jsonl_file_is_selected(
    provider: CaptureProvider,
    path: &Path,
    antigravity_full_transcript_is_regular: bool,
) -> bool {
    if provider == CaptureProvider::FactoryAiDroid {
        return super::native_path::factory_droid_file_is_selected(path);
    }
    if provider == CaptureProvider::QwenCode {
        return super::native_path::qwen_code_file_is_selected(path);
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
        || !provider_jsonl_path_is_native(provider, path)
    {
        return false;
    }
    if provider != CaptureProvider::Antigravity
        || path.file_name().and_then(|name| name.to_str()) != Some("transcript.jsonl")
    {
        return true;
    }
    !antigravity_full_transcript_is_regular
}
pub(super) fn native_jsonl_record_starts_session(provider: CaptureProvider, value: &Value) -> bool {
    matches!(
        provider,
        CaptureProvider::Antigravity | CaptureProvider::Windsurf
    ) || native_jsonl_header_session_id(provider, value).is_some()
}

pub(super) fn validate_direct_native_jsonl_provider(provider: CaptureProvider) -> Result<()> {
    if matches!(
        provider,
        CaptureProvider::Antigravity
            | CaptureProvider::Gemini
            | CaptureProvider::Tabnine
            | CaptureProvider::FactoryAiDroid
            | CaptureProvider::Windsurf
            | CaptureProvider::Qoder
            | CaptureProvider::CopilotCli
            | CaptureProvider::QwenCode
    ) {
        return Ok(());
    }
    Err(CaptureError::InvalidPayload(format!(
        "{} does not use the direct native JSONL batch driver",
        provider.as_str()
    )))
}
