use std::path::{Path, PathBuf};

use super::CodexPromptHistorySourceBackedResultV0;

pub(super) fn absolute_lexical_path(
    path: &Path,
) -> CodexPromptHistorySourceBackedResultV0<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}
