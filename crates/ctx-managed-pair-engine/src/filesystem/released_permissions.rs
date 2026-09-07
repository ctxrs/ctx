//! Bounded ACL adaptation for the released installed-Core installer entry.

use std::{fs, os::windows::fs::MetadataExt as _, path::Path};

use anyhow::{Context as _, Result, bail};
use ctx_history_platform::platform_security::{
    restrict_private_directory, restrict_private_file_handle,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_READ, READ_CONTROL, SYNCHRONIZE,
    WRITE_DAC,
};

use super::{
    Entry, Layout, Slot, validate_absolute_root, validate_open_owner_regular,
    windows_file_information,
};

/// Protects only the fixed paths left with inherited ACLs by released Windows
/// installers. The caller must hold the installation lock and have verified
/// the installed Core against the signed candidate and its install marker.
///
/// This is not pair validation or publication: bytes, ownership, and rollback
/// witnesses are preserved for the ordinary transaction engine to validate.
/// Unknown descendants and transaction files are never traversed or repaired.
pub fn protect_released_managed_pair_under_installation_lock(root: &Path) -> Result<()> {
    validate_absolute_root(root, "released managed-pair root")?;
    restrict_private_directory(root).context("protect released managed-pair root")?;
    for relative in ["libexec", "share", "share/ctx"] {
        let path = root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(_) => restrict_private_directory(&path)
                .with_context(|| format!("protect released managed-pair directory {relative}"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspect released managed-pair directory"),
        }
    }
    // bin is already certified by the installation lock. Missing directories
    // use the normal private-creation owner, then every destination is bound
    // through its retained no-follow directory handle.
    let layout = Layout::open(root, true)?;
    for slot in [Slot::Companion, Slot::Marker, Slot::Envelope, Slot::State] {
        let entry = layout.target(slot);
        if entry
            .directory
            .entry_metadata(&entry.name, entry.path())?
            .is_some()
        {
            protect_existing(&entry, slot.label())?;
        }
    }
    layout.revalidate()
}

fn protect_existing(entry: &Entry, label: &str) -> Result<()> {
    let file = entry.directory.open_relative(
        &entry.name,
        FILE_GENERIC_READ | READ_CONTROL | WRITE_DAC | SYNCHRONIZE,
        FILE_SHARE_READ,
        windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
    )?;
    let metadata = file.metadata()?;
    let (device, identity, links) = windows_file_information(&file, label)?;
    let named = entry.directory.entry_metadata(&entry.name, entry.path())?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || links != 1
        || !named.is_some_and(|named| {
            named.is_file && !named.is_symlink && named.device == device && named.file == identity
        })
    {
        bail!("released {label} is not a unique no-follow Windows file");
    }
    // The retained handle denies write/delete sharing. Hard links are rejected
    // before touching their shared security descriptor; the platform authority
    // rejects foreign ownership and changes only this handle's DACL.
    restrict_private_file_handle(&file).with_context(|| format!("protect released {label}"))?;
    validate_open_owner_regular(entry, &file, label)
}
