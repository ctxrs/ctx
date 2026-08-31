use std::io::Read as _;

use super::*;

struct PointerFileSnapshot {
    file: Option<File>,
    identity: FileIdentity,
    bytes: Vec<u8>,
}

/// Ownership of the predecessor handle after its terminal validation.
///
/// Windows replacement consumes this value so the validated handle cannot be
/// released before the prepared `MoveFileExW` call is ready to execute.
#[cfg(windows)]
pub(crate) struct ValidatedPredecessorPointer {
    _file: Option<File>,
}

#[cfg(not(windows))]
fn open_active_pointer_fence(path: &Path) -> Result<(File, FileIdentity)> {
    open_regular_file(path)
}

#[cfg(windows)]
fn open_active_pointer_fence(path: &Path) -> Result<(File, FileIdentity)> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

    validate_named_regular_file(path)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| IndexError::ChecksumMismatch)?;
    let identity = file_identity(&file).map_err(|_| IndexError::ChecksumMismatch)?;
    validate_named_regular_file(path)?;
    let named = open_nofollow(path).map_err(|_| IndexError::ChecksumMismatch)?;
    if file_identity(&named).map_err(|_| IndexError::ChecksumMismatch)? != identity {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok((file, identity))
}

/// Exact durable predecessor authority retained from writer admission through
/// candidate activation. An incompatible pointer remains opaque but is still
/// fenced by its native control-file identity.
pub struct ActiveGenerationPointerFence {
    expected: Option<PointerFileSnapshot>,
    topology_authority: Option<ActiveGenerationPointer>,
}

impl ActiveGenerationPointerFence {
    /// Captures the current pointer without decoding an unsupported version.
    /// A present pointer may be opaque only when the normal loader independently
    /// classifies that exact native file as version-incompatible.
    #[doc(hidden)]
    pub fn capture(
        root: &Path,
        topology_authority: Option<&ActiveGenerationPointer>,
    ) -> Result<Self> {
        ensure_real_directory(root)?;
        let path = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let expected = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
            Ok(_) => {
                let (mut file, identity) = open_active_pointer_fence(&path)?;
                let length =
                    usize::try_from(identity.length()).map_err(|_| IndexError::CountOverflow)?;
                let mut bytes = Vec::with_capacity(length);
                file.read_to_end(&mut bytes)?;
                if bytes.len() != length {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                if let Some(pointer) = topology_authority {
                    if serde_json::to_vec(pointer)? != bytes {
                        return Err(IndexError::ConcurrentGenerationChange);
                    }
                } else if !matches!(
                    load_active_generation_pointer(root),
                    Err(IndexError::UnsupportedActiveGenerationPointer(_))
                ) {
                    return Err(IndexError::ConcurrentGenerationChange);
                }
                Some(PointerFileSnapshot {
                    file: Some(file),
                    identity,
                    bytes,
                })
            }
        };
        if topology_authority.is_some() && expected.is_none() {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let fence = Self {
            expected,
            topology_authority: topology_authority.cloned(),
        };
        fence.validate(root)?;
        Ok(fence)
    }

    pub(crate) fn topology_authority(&self) -> Option<&ActiveGenerationPointer> {
        self.topology_authority.as_ref()
    }

    pub fn validate(&self, root: &Path) -> Result<()> {
        let path = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        self.validate_path(&path)
    }

    fn validate_path(&self, path: &Path) -> Result<()> {
        let Some(expected) = &self.expected else {
            return match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(IndexError::ConcurrentGenerationChange),
            };
        };
        let file = expected
            .file
            .as_ref()
            .ok_or(IndexError::ConcurrentGenerationChange)?;
        if file_identity(file).map_err(|_| IndexError::ConcurrentGenerationChange)?
            != expected.identity
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let (mut current_file, current) =
            open_regular_file(path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
        if current != expected.identity {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let length = usize::try_from(current.length())
            .map_err(|_| IndexError::ConcurrentGenerationChange)?;
        let mut bytes = Vec::with_capacity(length);
        current_file
            .read_to_end(&mut bytes)
            .map_err(|_| IndexError::ConcurrentGenerationChange)?;
        if bytes.len() != length || bytes != expected.bytes {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        #[cfg(all(test, not(windows)))]
        run_pointer_observation_test_hook(path);
        if file_identity(file).map_err(|_| IndexError::ConcurrentGenerationChange)?
            != expected.identity
            || file_identity(&current_file).map_err(|_| IndexError::ConcurrentGenerationChange)?
                != expected.identity
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let (mut named_file, named_identity) =
            open_regular_file(path).map_err(|_| IndexError::ConcurrentGenerationChange)?;
        if named_identity != expected.identity {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        let named_length = usize::try_from(named_identity.length())
            .map_err(|_| IndexError::ConcurrentGenerationChange)?;
        let mut named_bytes = Vec::with_capacity(named_length);
        named_file
            .read_to_end(&mut named_bytes)
            .map_err(|_| IndexError::ConcurrentGenerationChange)?;
        if named_bytes.len() != named_length || named_bytes != expected.bytes {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        if file_identity(file).map_err(|_| IndexError::ConcurrentGenerationChange)?
            != expected.identity
            || file_identity(&named_file).map_err(|_| IndexError::ConcurrentGenerationChange)?
                != expected.identity
        {
            return Err(IndexError::ConcurrentGenerationChange);
        }
        Ok(())
    }

    /// Performs the final pointer identity/value/no-follow validation and
    /// transfers ownership of the retained handle to the replacement syscall.
    #[cfg(windows)]
    pub(crate) fn terminal_validate(
        &mut self,
        pointer_path: &Path,
    ) -> Result<ValidatedPredecessorPointer> {
        self.validate_path(pointer_path)?;
        let file = self
            .expected
            .as_mut()
            .map(|expected| {
                expected
                    .file
                    .take()
                    .ok_or(IndexError::ConcurrentGenerationChange)
            })
            .transpose()?;
        Ok(ValidatedPredecessorPointer { _file: file })
    }
}

#[cfg(all(test, not(windows)))]
enum PointerObservationMutation {
    Rewrite(Vec<u8>),
    Truncate,
}

#[cfg(all(test, not(windows)))]
thread_local! {
    static POINTER_OBSERVATION_MUTATION: std::cell::RefCell<Option<PointerObservationMutation>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, not(windows)))]
struct PointerObservationMutationGuard;

#[cfg(all(test, not(windows)))]
impl PointerObservationMutationGuard {
    fn install(mutation: PointerObservationMutation) -> Self {
        POINTER_OBSERVATION_MUTATION.with(|active| *active.borrow_mut() = Some(mutation));
        Self
    }
}

#[cfg(all(test, not(windows)))]
impl Drop for PointerObservationMutationGuard {
    fn drop(&mut self) {
        POINTER_OBSERVATION_MUTATION.with(|active| active.borrow_mut().take());
    }
}

#[cfg(all(test, not(windows)))]
fn run_pointer_observation_test_hook(path: &Path) {
    POINTER_OBSERVATION_MUTATION.with(|active| match active.borrow_mut().take() {
        Some(PointerObservationMutation::Rewrite(bytes)) => fs::write(path, bytes).unwrap(),
        Some(PointerObservationMutation::Truncate) => fs::write(path, b"").unwrap(),
        None => {}
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(digit: char) -> ActiveGenerationPointer {
        let digit = digit.to_string();
        ActiveGenerationPointer::new(
            GenerationSlot::new(
                digit.repeat(64),
                format!("generation-{}", digit.repeat(32)),
                digit.repeat(64),
            )
            .unwrap(),
            None,
        )
        .unwrap()
    }

    fn write_pointer(root: &Path, pointer: &ActiveGenerationPointer) {
        fs::write(
            root.join(crate::ACTIVE_GENERATION_POINTER_FILE),
            serde_json::to_vec(pointer).unwrap(),
        )
        .unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn predecessor_fence_fails_closed_for_replacement() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let root = temporary_directory.path();
        let first = pointer('1');
        write_pointer(root, &first);
        let fence = ActiveGenerationPointerFence::capture(root, Some(&first)).unwrap();
        let target = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let staged = root.join("active-generation.staged");
        fs::write(&staged, serde_json::to_vec(&pointer('2')).unwrap()).unwrap();
        fs::rename(&staged, target).unwrap();

        assert!(matches!(
            fence.validate(root),
            Err(IndexError::ConcurrentGenerationChange)
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn predecessor_fence_rechecks_identity_after_same_length_rewrite_and_truncation() {
        for mutation in ["rewrite", "truncation"] {
            let temporary_directory = tempfile::tempdir().unwrap();
            let root = temporary_directory.path();
            let first = pointer('1');
            write_pointer(root, &first);
            let fence = ActiveGenerationPointerFence::capture(root, Some(&first)).unwrap();
            let first_bytes = serde_json::to_vec(&first).unwrap();
            let observation_mutation = if mutation == "rewrite" {
                let mut replacement = first_bytes.clone();
                let index = replacement
                    .iter()
                    .position(|byte| *byte == b'1')
                    .expect("pointer contains a generation digit");
                replacement[index] = b'3';
                assert_eq!(replacement.len(), first_bytes.len());
                PointerObservationMutation::Rewrite(replacement)
            } else {
                PointerObservationMutation::Truncate
            };
            let hook = PointerObservationMutationGuard::install(observation_mutation);

            assert!(matches!(
                fence.validate(root),
                Err(IndexError::ConcurrentGenerationChange)
            ));
            drop(hook);
        }
    }

    #[test]
    fn predecessor_fence_fails_closed_for_absent_predecessor() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let root = temporary_directory.path();
        let absent = ActiveGenerationPointerFence::capture(root, None).unwrap();
        write_pointer(root, &pointer('1'));
        assert!(matches!(
            absent.validate(root),
            Err(IndexError::ConcurrentGenerationChange)
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn predecessor_fence_fails_closed_for_opaque_predecessor() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let root = temporary_directory.path();
        fs::write(
            root.join(crate::ACTIVE_GENERATION_POINTER_FILE),
            br#"{"version":1}"#,
        )
        .unwrap();
        let opaque = ActiveGenerationPointerFence::capture(root, None).unwrap();
        write_pointer(root, &pointer('2'));
        assert!(matches!(
            opaque.validate(root),
            Err(IndexError::ConcurrentGenerationChange)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn predecessor_fence_rejects_named_reparse_substitution() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let root = temporary_directory.path();
        let first = pointer('1');
        write_pointer(root, &first);
        let fence = ActiveGenerationPointerFence::capture(root, Some(&first)).unwrap();
        let target = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let replacement = root.join("active-generation.replacement");
        fs::write(&replacement, serde_json::to_vec(&pointer('2')).unwrap()).unwrap();
        fs::remove_file(&target).unwrap();
        std::os::unix::fs::symlink(&replacement, &target).unwrap();

        assert!(matches!(
            fence.validate(root),
            Err(IndexError::ConcurrentGenerationChange)
        ));
    }

    #[cfg(windows)]
    fn assert_no_temporary_files(root: &Path) {
        assert!(fs::read_dir(root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".ctx-tantivy-atomic-")));
    }

    #[cfg(windows)]
    #[test]
    fn validated_predecessor_token_denies_write_delete_and_replacement() {
        let temporary_directory = tempfile::tempdir().unwrap();
        let root = temporary_directory.path();
        let predecessor = pointer('1');
        let successor_bytes = serde_json::to_vec(&pointer('2')).unwrap();
        write_pointer(root, &predecessor);
        let target = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let staged = root.join("active-generation.staged");
        fs::write(&staged, &successor_bytes).unwrap();
        let mut fence = ActiveGenerationPointerFence::capture(root, Some(&predecessor)).unwrap();
        let token = fence.terminal_validate(&target).unwrap();

        let write_error = OpenOptions::new().write(true).open(&target).unwrap_err();
        assert_eq!(write_error.raw_os_error(), Some(5));
        assert_eq!(
            fs::remove_file(&target).unwrap_err().raw_os_error(),
            Some(5)
        );
        assert_eq!(
            fs::rename(&staged, &target).unwrap_err().raw_os_error(),
            Some(5)
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            serde_json::to_vec(&predecessor).unwrap()
        );

        drop(token);
        fs::rename(&staged, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), successor_bytes);
    }

    #[cfg(windows)]
    #[test]
    fn fenced_publication_releases_predecessor_and_reports_external_blockers() {
        use std::os::windows::fs::OpenOptionsExt as _;

        const FILE_SHARE_READ: u32 = 0x0000_0001;

        let temporary_directory = tempfile::tempdir().unwrap();
        let root = temporary_directory.path();
        ctx_history_platform::platform_security::ensure_private_directory(root).unwrap();
        let predecessor = pointer('1');
        let successor = pointer('2');
        let retry_successor = pointer('3');
        write_pointer(root, &predecessor);
        let mut fence = ActiveGenerationPointerFence::capture(root, Some(&predecessor)).unwrap();

        assert!(matches!(
            crate::publish_active_generation_pointer_validated_predecessor_fence(
                root,
                &successor,
                &mut fence,
                |fence| fence.validate(root),
            )
            .unwrap(),
            crate::PointerPublicationOutcome::Durable
        ));
        assert_eq!(
            load_active_generation_pointer(root).unwrap(),
            Some(successor.clone())
        );
        assert_no_temporary_files(root);
        let target = root.join(crate::ACTIVE_GENERATION_POINTER_FILE);
        let blocker = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&target)
            .unwrap();
        let mut fence = ActiveGenerationPointerFence::capture(root, Some(&successor)).unwrap();

        let error = crate::publish_active_generation_pointer_validated_predecessor_fence(
            root,
            &retry_successor,
            &mut fence,
            |fence| fence.validate(root),
        )
        .unwrap_err();
        assert!(matches!(error, IndexError::Io(ref error) if error.raw_os_error() == Some(5)));
        assert_eq!(
            fs::read(&target).unwrap(),
            serde_json::to_vec(&successor).unwrap()
        );
        assert_no_temporary_files(root);

        drop(blocker);
        let mut fence = ActiveGenerationPointerFence::capture(root, Some(&successor)).unwrap();
        assert!(matches!(
            crate::publish_active_generation_pointer_validated_predecessor_fence(
                root,
                &retry_successor,
                &mut fence,
                |fence| fence.validate(root),
            )
            .unwrap(),
            crate::PointerPublicationOutcome::Durable
        ));
        assert_eq!(
            load_active_generation_pointer(root).unwrap(),
            Some(retry_successor)
        );
    }
}
