use std::path::Path;

use anyhow::{bail, Result};
use ctx_history_capture::MAX_PROVIDER_ROOT_SELECTOR_BYTES;
use ctx_history_core::CaptureProvider;

pub(super) fn validate_provider_root_support(provider: CaptureProvider) -> Result<()> {
    if matches!(provider, CaptureProvider::Claude | CaptureProvider::Codex) {
        return Ok(());
    }
    bail!(
        "configured provider homes currently support only claude and codex, not {}",
        provider.as_str()
    )
}

pub(super) fn validate_root_selector(kind: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= MAX_PROVIDER_ROOT_SELECTOR_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        return Ok(());
    }
    bail!(
        "{kind} `{value}` must be 1..={MAX_PROVIDER_ROOT_SELECTOR_BYTES} ASCII letters, digits, hyphens, or underscores"
    )
}

pub(super) fn validate_provider_root_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.to_str().is_none()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        bail!(
            "configured provider home must be a normalized absolute UTF-8 path: {}",
            path.display()
        );
    }
    Ok(())
}
