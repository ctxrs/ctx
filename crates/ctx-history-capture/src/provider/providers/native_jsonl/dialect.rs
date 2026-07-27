use std::{fs, path::Path};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::{CaptureError, Result};

use super::normalization::native_jsonl_header_session_id;

pub(crate) fn native_jsonl_missing_reason(provider: CaptureProvider) -> &'static str {
    match provider {
        CaptureProvider::Pi => "no Pi session JSONL files found",
        CaptureProvider::Claude => "no Claude project session JSONL files found",
        CaptureProvider::Antigravity => {
            "no Antigravity transcript JSONL files found under brain/*/.system_generated/logs"
        }
        CaptureProvider::Gemini => "no Gemini CLI chat JSONL transcripts found under chats",
        CaptureProvider::Tabnine => "no Tabnine CLI chat JSONL transcripts found under chats",
        CaptureProvider::Windsurf => {
            "no Windsurf Cascade hook transcript JSONL files found under ~/.windsurf/transcripts"
        }
        CaptureProvider::Qoder => {
            "no Qoder transcript JSONL files found under ~/.qoder/projects/*/transcript"
        }
        CaptureProvider::CopilotCli => "no Copilot CLI session events.jsonl transcripts found",
        CaptureProvider::FactoryAiDroid => "no Factory AI Droid session JSONL transcripts found",
        CaptureProvider::QwenCode => "no Qwen Code chat JSONL transcripts found under chats",
        CaptureProvider::KimiCodeCli => "no Kimi Code CLI wire.jsonl transcripts found",
        CaptureProvider::MistralVibe => {
            "no Mistral Vibe meta.json/messages.jsonl session directories found"
        }
        CaptureProvider::Mux => "no Mux chat.jsonl or partial.json session files found",
        _ => "no native provider JSONL transcripts found",
    }
}

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

pub(super) fn native_jsonl_file_is_selected(provider: CaptureProvider, path: &Path) -> bool {
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
    fs::symlink_metadata(path.with_file_name("transcript_full.jsonl"))
        .map(|metadata| !metadata.file_type().is_file())
        .unwrap_or(true)
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
