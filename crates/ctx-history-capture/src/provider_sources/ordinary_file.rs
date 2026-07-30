use std::{
    fs::File,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    Result,
};

#[cfg(test)]
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

#[cfg(test)]
static FORBIDDEN_CONTENT_OPENS: LazyLock<Mutex<BTreeSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

#[cfg(test)]
pub(crate) struct ForbiddenOrdinaryFileContentOpen {
    path: PathBuf,
}

#[cfg(test)]
impl Drop for ForbiddenOrdinaryFileContentOpen {
    fn drop(&mut self) {
        let mut paths = FORBIDDEN_CONTENT_OPENS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        paths.remove(&self.path);
    }
}

#[cfg(test)]
pub(crate) fn forbid_ordinary_file_content_open(path: &Path) -> ForbiddenOrdinaryFileContentOpen {
    let path = path.to_path_buf();
    let mut paths = FORBIDDEN_CONTENT_OPENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    paths.insert(path.clone());
    ForbiddenOrdinaryFileContentOpen { path }
}

#[cfg(test)]
fn reject_forbidden_content_open(path: &Path) -> Result<()> {
    let paths = FORBIDDEN_CONTENT_OPENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if paths.contains(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "test forbids opening this provider transcript",
        )
        .into());
    }
    Ok(())
}

/// A bounded observation of an ordinary provider file.
///
/// Length and mtime retain the inexpensive append/no-op checks used by callers.
/// The token is the root-handle layer's fixed-width fingerprint of the exact
/// opened object and its change stamp. The same opened-handle proof is
/// revalidated before the observation escapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrdinaryFileObservation {
    len: u64,
    modified_at: SystemTime,
    token: [u8; 32],
}

impl OrdinaryFileObservation {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    pub fn token(&self) -> &[u8; 32] {
        &self.token
    }

    pub fn token_hex(&self) -> String {
        self.token
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

pub fn observe_ordinary_file(path: impl AsRef<Path>) -> Result<OrdinaryFileObservation> {
    observe_ordinary_file_inner(path.as_ref(), || {})
}

fn observe_ordinary_file_inner(
    path: &Path,
    before_open: impl FnOnce(),
) -> Result<OrdinaryFileObservation> {
    before_open();
    #[cfg(test)]
    reject_forbidden_content_open(path)?;
    let opened = open_provider_source_file(path)?;
    observe_opened_ordinary_file(path, &opened)
}

pub(crate) fn observe_opened_ordinary_file(
    _path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<OrdinaryFileObservation> {
    let token = opened.ordinary_file_token();
    opened.revalidate_leaf()?;

    Ok(OrdinaryFileObservation {
        len: opened.len(),
        modified_at: opened.modified().unwrap_or(UNIX_EPOCH),
        token,
    })
}

pub(crate) fn open_ordinary_file_without_following(path: &Path) -> Result<File> {
    #[cfg(test)]
    reject_forbidden_content_open(path)?;
    open_provider_source_file(path)?
        .file()
        .try_clone()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Seek, SeekFrom, Write},
        time::Duration,
    };

    use super::*;

    #[test]
    fn observation_token_is_derived_from_the_opened_object_stamp() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        std::fs::write(&path, b"content\n").unwrap();
        let opened = open_provider_source_file(&path).unwrap();

        let observation = observe_opened_ordinary_file(&path, &opened).unwrap();

        assert_eq!(observation.token(), &opened.ordinary_file_token());
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn opened_authority_fingerprint_detects_same_size_rewrite_with_restored_mtime() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let source = vec![b'a'; 128 * 1024];
        std::fs::write(&path, source).unwrap();
        let original_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let first = observe_ordinary_file(&path).unwrap();

        std::thread::sleep(Duration::from_millis(2));
        let mut file = File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(16 * 1024)).unwrap();
        file.write_all(b"b").unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(original_modified))
            .unwrap();
        drop(file);
        let second = observe_ordinary_file(&path).unwrap();

        assert_eq!(first.len(), second.len());
        assert_eq!(first.modified_at(), second.modified_at());
        assert_ne!(first.token(), second.token());
    }

    #[test]
    fn opened_observation_rejects_named_replacement() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        std::fs::write(&path, b"original\n").unwrap();
        let opened = open_provider_source_file(&path).unwrap();

        std::fs::rename(&path, &moved).unwrap();
        std::fs::write(&path, b"replacement\n").unwrap();

        assert!(observe_opened_ordinary_file(&path, &opened).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_final_component() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target = temp.path().join("target.jsonl");
        let link = temp.path().join("link.jsonl");
        std::fs::write(&target, b"content\n").unwrap();
        symlink(&target, &link).unwrap();

        assert!(observe_ordinary_file(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_a_symlinked_parent_component() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let target_parent = temp.path().join("target-parent");
        let link_parent = temp.path().join("link-parent");
        std::fs::create_dir(&target_parent).unwrap();
        std::fs::write(target_parent.join("source.jsonl"), b"content\n").unwrap();
        symlink(&target_parent, &link_parent).unwrap();

        assert!(observe_ordinary_file(link_parent.join("source.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn observation_rejects_final_component_symlink_swapped_before_open() {
        use std::os::unix::fs::symlink;

        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        let moved = temp.path().join("moved.jsonl");
        let target = temp.path().join("target.jsonl");
        std::fs::write(&path, b"original\n").unwrap();
        std::fs::write(&target, b"replacement\n").unwrap();

        let result = observe_ordinary_file_inner(&path, || {
            std::fs::rename(&path, &moved).unwrap();
            symlink(&target, &path).unwrap();
        });

        assert!(result.is_err());
    }
}
