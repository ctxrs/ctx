use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;

use crate::Result;

pub(super) struct Inventory {
    pub(super) paths: BTreeSet<PathBuf>,
}

pub(super) fn discover_inventory(root: &Path) -> Result<Inventory> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Inventory {
                paths: BTreeSet::new(),
            });
        }
        Err(error) => return Err(error.into()),
    }
    let restrict_to_sessions = root.is_dir();
    let mut paths = BTreeSet::new();
    crate::provider::providers::native_jsonl::visit_native_jsonl_files(
        root,
        CaptureProvider::OpenClaw,
        &mut |candidate| {
            if restrict_to_sessions && !path_has_component(candidate, "sessions") {
                return Ok(());
            }
            paths.insert(fs::canonicalize(candidate)?);
            Ok(())
        },
    )?;
    Ok(Inventory { paths })
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}
