use std::{
    fs::Metadata,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{common::io::path_has_component, CaptureError, Result};

pub(super) const OPENHANDS_MAX_PATH_BYTES: usize = 7 * 1024;

/// Legacy wire evidence retained only for stable OpenHands leaf locators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct OpenHandsObservedTime {
    pub(super) before_epoch: bool,
    pub(super) seconds: u64,
    pub(super) nanos: u32,
}

impl OpenHandsObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

/// The serialized V1 observation remains a locator input even though source
/// no-op detection now uses the stronger shared ordinary-file observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OpenHandsFileObservation {
    pub(super) length: u64,
    pub(super) modified: OpenHandsObservedTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
}

impl OpenHandsFileObservation {
    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: OpenHandsObservedTime::from_system_time(metadata.modified()?),
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }
}

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
