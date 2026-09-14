use std::{io, path::Path};

use crate::certification::{ActiveGenerationPointerFence, ValidatedPredecessorPointer};

use super::*;

impl DurableMmapDirectory {
    pub(crate) fn atomic_write_with_outcome_validated_predecessor_fence<F>(
        &self,
        path: &Path,
        data: &[u8],
        predecessor_fence: &mut ActiveGenerationPointerFence,
        validate_before_replace: F,
    ) -> crate::Result<DurableAtomicWriteOutcome>
    where
        F: FnOnce(&ActiveGenerationPointerFence) -> crate::Result<()>,
    {
        let target_path = self.resolve_path(path);
        let parent_path = target_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path {} has no parent directory", target_path.display()),
            )
        })?;
        let parent_sync = ParentDirectorySync::open(parent_path)?;
        atomic_replace_with_outcome_validated_predecessor_fence(
            &target_path,
            data,
            move || parent_sync.sync(),
            predecessor_fence,
            validate_before_replace,
        )
    }
}

pub(super) struct WindowsAtomicReplacement {
    source_wide: Vec<u16>,
    target_wide: Vec<u16>,
}

impl WindowsAtomicReplacement {
    pub(super) fn prepare(source: &Path, target: &Path) -> io::Result<Self> {
        Ok(Self {
            source_wide: nul_terminated(source)?,
            target_wide: nul_terminated(target)?,
        })
    }

    pub(super) fn replace(self) -> io::Result<()> {
        let source = self.source_wide.as_ptr();
        let target = self.target_wide.as_ptr();
        // SAFETY: both prepared path buffers remain alive for the call.
        let moved = unsafe {
            move_file_ex_w(
                source,
                target,
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn replace_validated(self, predecessor: ValidatedPredecessorPointer) -> io::Result<()> {
        let source = self.source_wide.as_ptr();
        let target = self.target_wide.as_ptr();
        // SAFETY: the prepared buffers own every resource needed by the
        // syscall and remain alive across this release boundary and call.
        drop(predecessor);
        let moved = unsafe {
            move_file_ex_w(
                source,
                target,
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

pub(super) fn atomic_replace_with_outcome_validated_predecessor_fence<SyncParent, Validate>(
    target_path: &Path,
    data: &[u8],
    sync_parent: SyncParent,
    predecessor_fence: &mut ActiveGenerationPointerFence,
    validate_before_replace: Validate,
) -> crate::Result<DurableAtomicWriteOutcome>
where
    SyncParent: FnOnce() -> io::Result<()>,
    Validate: FnOnce(&ActiveGenerationPointerFence) -> crate::Result<()>,
{
    let temporary_path = prepare_atomic_write(target_path, data)?;
    let replacement = match WindowsAtomicReplacement::prepare(&temporary_path, target_path) {
        Ok(replacement) => replacement,
        Err(error) => {
            let _ = fs::remove_file(&temporary_path);
            return Err(error.into());
        }
    };
    atomic_write_checkpoint(
        AtomicWriteStage::AfterTemporarySyncBeforeReplace,
        target_path,
    )?;
    atomic_write_checkpoint(AtomicWriteStage::BeforeReplace, target_path)?;

    if let Err(error) = validate_before_replace(predecessor_fence) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    let predecessor = match predecessor_fence.terminal_validate(target_path) {
        Ok(predecessor) => predecessor,
        Err(error) => {
            let _ = fs::remove_file(&temporary_path);
            return Err(error);
        }
    };
    if let Err(error) = replacement.replace_validated(predecessor) {
        return Err(failed_atomic_replacement(
            &temporary_path,
            target_path,
            error,
        ));
    }
    Ok(finish_atomic_write(target_path, sync_parent))
}

fn nul_terminated(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt as _;

    let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if path_wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows path contains an interior NUL",
        ));
    }
    path_wide.push(0);
    Ok(path_wide)
}

const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "MoveFileExW"]
    fn move_file_ex_w(existing_file_name: *const u16, new_file_name: *const u16, flags: u32)
        -> i32;
}
