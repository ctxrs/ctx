fn validate_path_identity(
    path: &Path,
    expected: &ProviderFileStableIdentity,
    identity: &impl Fn(&File, &std::fs::Metadata) -> Option<ProviderFileStableIdentity>,
) -> Result<std::result::Result<(), ProviderJsonlReplacementReason>> {
    let current = match open_regular_provider_transcript_file(path) {
        Ok(file) => file,
        Err(crate::CaptureError::InvalidProviderTranscriptPath { .. }) => {
            return Ok(Err(ProviderJsonlReplacementReason::StableIdentityChanged));
        }
        Err(error) => return Err(error),
    };
    let metadata = current.metadata()?;
    Ok(validate_stable_identity(
        expected,
        identity(&current, &metadata),
    ))
}

fn validate_stable_identity(
    expected: &ProviderFileStableIdentity,
    actual: Option<ProviderFileStableIdentity>,
) -> std::result::Result<(), ProviderJsonlReplacementReason> {
    match actual {
        None => Err(ProviderJsonlReplacementReason::StableIdentityUnavailable),
        Some(actual) if &actual != expected => {
            Err(ProviderJsonlReplacementReason::StableIdentityChanged)
        }
        Some(_) => Ok(()),
    }
}

fn checkpoint_hashes(
    file: &mut File,
    committed_offset: u64,
    validated_checkpoint: Option<&ProviderJsonlAppendCheckpoint>,
) -> Result<std::result::Result<(String, String), ProviderJsonlReplacementReason>> {
    if let Some(checkpoint) = validated_checkpoint {
        if checkpoint.committed_offset > 0 {
            let Some(boundary_byte) = read_range_exact(file, checkpoint.committed_offset - 1, 1)?
            else {
                return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
            };
            if boundary_byte != b"\n" {
                return Ok(Err(
                    ProviderJsonlReplacementReason::BoundaryNotNewlineAligned,
                ));
            }
        }
        let Some(head_sha256) = hash_head_exact(file, checkpoint.committed_offset)? else {
            return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
        };
        if head_sha256 != checkpoint.head_sha256 {
            return Ok(Err(ProviderJsonlReplacementReason::HeadHashMismatch));
        }
        let Some(boundary_sha256) = hash_boundary_exact(file, checkpoint.committed_offset)? else {
            return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
        };
        if boundary_sha256 != checkpoint.boundary_sha256 {
            return Ok(Err(ProviderJsonlReplacementReason::BoundaryHashMismatch));
        }
    }

    if committed_offset > 0 {
        let Some(boundary_byte) = read_range_exact(file, committed_offset - 1, 1)? else {
            return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
        };
        if boundary_byte != b"\n" {
            return Ok(Err(
                ProviderJsonlReplacementReason::BoundaryNotNewlineAligned,
            ));
        }
    }

    let Some(head_sha256) = hash_head_exact(file, committed_offset)? else {
        return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
    };
    let Some(boundary_sha256) = hash_boundary_exact(file, committed_offset)? else {
        return Ok(Err(ProviderJsonlReplacementReason::FileShrank));
    };
    Ok(Ok((head_sha256, boundary_sha256)))
}

fn hash_head_exact(file: &mut File, boundary: u64) -> Result<Option<String>> {
    hash_range_exact(file, 0, boundary.min(SENTINEL_BYTES))
}

fn hash_boundary_exact(file: &mut File, boundary: u64) -> Result<Option<String>> {
    let start = boundary.saturating_sub(SENTINEL_BYTES);
    hash_range_exact(file, start, boundary - start)
}

fn hash_range_exact(file: &mut File, start: u64, bytes: u64) -> Result<Option<String>> {
    let Some(value) = read_range_exact(file, start, bytes)? else {
        return Ok(None);
    };
    let digest = Sha256::digest(value);
    Ok(Some(
        digest.iter().map(|byte| format!("{byte:02x}")).collect(),
    ))
}

fn read_range_exact(file: &mut File, start: u64, bytes: u64) -> Result<Option<Vec<u8>>> {
    file.seek(SeekFrom::Start(start))?;
    let mut value = vec![0; bytes.min(usize::MAX as u64) as usize];
    match file.read_exact(&mut value) {
        Ok(()) => Ok(Some(value)),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn stable_file_identity(
    _file: &File,
    metadata: &std::fs::Metadata,
) -> Option<ProviderFileStableIdentity> {
    use std::os::unix::fs::MetadataExt;

    Some(ProviderFileStableIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn stable_file_identity(
    file: &File,
    _metadata: &std::fs::Metadata,
) -> Option<ProviderFileStableIdentity> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandleEx, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut information = MaybeUninit::<FILE_ID_INFO>::zeroed();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            information.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let information = unsafe { information.assume_init() };
    Some(ProviderFileStableIdentity::Windows {
        volume_serial: information.VolumeSerialNumber,
        file_id: information.FileId.Identifier,
    })
}

#[cfg(not(any(unix, windows)))]
fn stable_file_identity(_: &File, _: &std::fs::Metadata) -> Option<ProviderFileStableIdentity> {
    None
}
