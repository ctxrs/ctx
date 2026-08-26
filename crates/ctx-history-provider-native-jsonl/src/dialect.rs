use std::{ffi::OsStr, path::Path};

use ctx_history_core::CaptureProvider;
use ctx_history_source_io::{BoundedTreeFileCandidate, OpenedProviderSourcePath};
use serde_json::Value;

use crate::{CaptureError, Result};

use super::normalization::native_jsonl_header_session_id;

fn provider_jsonl_path_is_native(provider: CaptureProvider, root: &Path, path: &Path) -> bool {
    match provider {
        CaptureProvider::Antigravity => {
            matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }
        CaptureProvider::Tabnine => path
            .components()
            .any(|component| component.as_os_str() == "chats"),
        CaptureProvider::Qoder => qoder_jsonl_path_is_native(root, path),
        CaptureProvider::CopilotCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }
        CaptureProvider::KimiCodeCli => {
            path.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path
                    .components()
                    .any(|component| component.as_os_str() == "agents")
        }
        CaptureProvider::GrokBuild => super::native_path::grok_build_file_is_selected(path),
        _ => true,
    }
}

fn qoder_jsonl_path_is_native(root: &Path, path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
        return false;
    }

    // An explicitly selected Qoder file retains its released path admission.
    // Directory sources instead name the projects transcript tree itself, so
    // classify its two native layouts relative to that selected authority.
    if root == path {
        return path
            .components()
            .any(|component| component.as_os_str() == "transcript")
            || path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                == Some(OsStr::new("projects"));
    }

    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    match (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) {
        (
            Some(std::path::Component::Normal(_project)),
            Some(std::path::Component::Normal(_session)),
            None,
            None,
        ) => true,
        (
            Some(std::path::Component::Normal(_project)),
            Some(std::path::Component::Normal(transcript)),
            Some(std::path::Component::Normal(_session)),
            None,
        ) => transcript == "transcript",
        _ => false,
    }
}

pub(super) fn native_jsonl_file_is_selected(
    provider: CaptureProvider,
    root: &Path,
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
        || !provider_jsonl_path_is_native(provider, root, path)
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

pub(super) fn native_jsonl_file_candidate_is_selected(
    provider: CaptureProvider,
    root: &Path,
    candidate: BoundedTreeFileCandidate<'_>,
) -> bool {
    let path = candidate.path();
    let full_transcript_is_regular = provider == CaptureProvider::Antigravity
        && path.file_name() == Some(OsStr::new("transcript.jsonl"))
        && candidate.parent().is_some_and(|directory| {
            matches!(
                directory.open_child(OsStr::new("transcript_full.jsonl")),
                Ok(OpenedProviderSourcePath::File(_))
            )
        });
    native_jsonl_file_is_selected(provider, root, path, full_transcript_is_regular)
}

pub(super) fn native_jsonl_record_starts_session(provider: CaptureProvider, value: &Value) -> bool {
    provider == CaptureProvider::Antigravity
        || native_jsonl_header_session_id(provider, value).is_some()
}

pub(super) fn validate_direct_native_jsonl_provider(provider: CaptureProvider) -> Result<()> {
    if matches!(
        provider,
        CaptureProvider::Antigravity
            | CaptureProvider::Tabnine
            | CaptureProvider::FactoryAiDroid
            | CaptureProvider::Qoder
            | CaptureProvider::CopilotCli
            | CaptureProvider::QwenCode
            | CaptureProvider::GrokBuild
    ) {
        return Ok(());
    }
    Err(CaptureError::InvalidPayload(format!(
        "{} does not use the direct native JSONL batch driver",
        provider.as_str()
    )))
}
