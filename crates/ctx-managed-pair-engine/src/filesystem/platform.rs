use std::fs::File;

#[cfg(not(unix))]
use anyhow::bail;
use anyhow::{anyhow, Context, Result};

#[cfg(windows)]
use super::{open_owner_regular_for_delete, require_file_identity};
use super::{require_stamp, Entry, FileStamp};

#[cfg(unix)]
pub(super) fn file_information(file: &File, _label: &str) -> Result<(u64, u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino(), metadata.len()))
}

#[cfg(windows)]
pub(super) fn file_information(file: &File, label: &str) -> Result<(u64, u64, u64)> {
    let (device, identity, _) = windows_file_information(file, label)?;
    Ok((device, identity, file.metadata()?.len()))
}

#[cfg(windows)]
pub(super) fn windows_file_information(file: &File, label: &str) -> Result<(u64, u64, u32)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
    };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("inspect {label}"));
    }
    let information = unsafe { information.assume_init() };
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        information.nNumberOfLinks,
    ))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn file_information(_file: &File, _label: &str) -> Result<(u64, u64, u64)> {
    bail!("managed-pair file identity is unsupported on this platform")
}

#[cfg(unix)]
pub(super) fn durable_rename(
    source_entry: &Entry,
    target_entry: &Entry,
    _expected: &FileStamp,
    _label: &str,
    _replace: bool,
) -> Result<()> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt as _, io::AsRawFd as _},
    };
    let source = CString::new(source_entry.name.as_bytes())
        .map_err(|_| anyhow!("managed-pair source name contains a NUL"))?;
    let target = CString::new(target_entry.name.as_bytes())
        .map_err(|_| anyhow!("managed-pair target name contains a NUL"))?;
    if unsafe {
        libc::renameat(
            source_entry.directory.file.as_raw_fd(),
            source.as_ptr(),
            target_entry.directory.file.as_raw_fd(),
            target.as_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("rename managed-pair file");
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn durable_rename(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    label: &str,
    replace: bool,
) -> Result<()> {
    use std::{
        mem::size_of,
        os::windows::{ffi::OsStrExt as _, io::AsRawHandle as _},
    };
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FileRenameInformation, NtSetInformationFile, FILE_RENAME_INFORMATION,
        },
        Win32::{Foundation::RtlNtStatusToDosError, System::IO::IO_STATUS_BLOCK},
    };

    let file = open_owner_regular_for_delete(source, label)?;
    require_file_identity(&file, expected, label)?;
    let name: Vec<u16> = target.name.encode_wide().collect();
    if name.is_empty() || name.contains(&0) {
        bail!("managed-pair target name is invalid");
    }
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| anyhow!("managed-pair target name is too long"))?;
    // Windows documents FileNameLength without the terminator, while the
    // FILE_RENAME_INFORMATION buffer itself must include its trailing WCHAR
    // storage.
    // The zero-filled tail therefore supplies the required terminator.
    let total_bytes = size_of::<FILE_RENAME_INFORMATION>()
        .checked_add(name_bytes)
        .ok_or_else(|| anyhow!("managed-pair rename buffer is too large"))?;
    let words = total_bytes.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    unsafe {
        (*information).Anonymous.ReplaceIfExists = replace;
        (*information).RootDirectory = target.directory.file.as_raw_handle().cast();
        (*information).FileNameLength = u32::try_from(name_bytes)?;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            name.len(),
        );
    }
    let mut status_block = IO_STATUS_BLOCK::default();
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle().cast(),
            &mut status_block,
            information.cast(),
            u32::try_from(total_bytes)?,
            FileRenameInformation,
        )
    };
    if status < 0 {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
        .context("rename managed-pair file by handle");
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn durable_replace(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, true)?;
    require_stamp(target, expected, max, label)
        .with_context(|| format!("revalidate renamed managed-pair {label}"))
}

#[cfg(unix)]
pub(crate) fn durable_replace(
    source: &Entry,
    target: &Entry,
    expected: &FileStamp,
    max: u64,
    label: &str,
) -> Result<()> {
    require_stamp(source, expected, max, label)?;
    durable_rename(source, target, expected, label, true)?;
    target.directory.sync()?;
    require_stamp(target, expected, max, label)
}

pub(super) fn sync_parent(entry: &Entry) -> Result<()> {
    entry.directory.sync()
}
