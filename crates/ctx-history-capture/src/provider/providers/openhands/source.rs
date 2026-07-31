use std::path::{Component, Path, PathBuf};

use crate::{common::io::path_has_component, CaptureError, Result};

pub(super) const OPENHANDS_MAX_PATH_BYTES: usize = 7 * 1024;

pub(super) fn normalized_openhands_authority_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(CaptureError::InvalidProviderTranscriptPath {
                        path: path.to_path_buf(),
                        reason: "OpenHands roots cannot escape the filesystem root",
                    });
                }
            }
        }
    }
    Ok(normalized)
}

pub(super) fn openhands_checked_path_text(path: &Path) -> Result<String> {
    let Some(text) = path.to_str() else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands selected paths must be valid UTF-8",
        });
    };
    if text.len() > OPENHANDS_MAX_PATH_BYTES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "OpenHands selected path exceeds the provider identity byte limit",
        });
    }
    Ok(text.to_owned())
}

pub(crate) fn openhands_json_path_is_event(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("json")
        && path_has_component(path, "v1_conversations")
}
