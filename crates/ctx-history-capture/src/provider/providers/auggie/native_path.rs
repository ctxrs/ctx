use std::path::{Component, Path, PathBuf};

use crate::{CaptureError, Result};

mod model;
mod parse;
mod source;
pub(crate) mod source_backed;

fn normalized_auggie_authority_path(path: &Path) -> Result<PathBuf> {
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
                        reason: "Auggie provider roots cannot escape the filesystem root",
                    });
                }
            }
        }
    }
    Ok(normalized)
}
