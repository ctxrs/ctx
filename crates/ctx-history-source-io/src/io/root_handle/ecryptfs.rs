//! eCryptfs inherits timestamps and statfs identity from its backing filesystem,
//! but replaces f_type. Its magic alone cannot qualify metadata change tokens.

use std::{fs::File, io::Read, os::unix::ffi::OsStringExt, path::PathBuf};

use super::{
    filesystem_stat, linux_filesystem_is_qualified, open_absolute_handle, AuthorityOpenError,
};

pub(super) const SUPER_MAGIC: i64 = 0xF15F;
const REJECTION: &str =
    "eCryptfs provider source roots require an inspectable qualified local backing filesystem";
const MAX_MOUNTINFO_BYTES: u64 = 1024 * 1024;

pub(super) fn qualify_backing(
    upper: &libc::statfs,
    mount_id: u64,
) -> Result<(), AuthorityOpenError> {
    let rejected = || AuthorityOpenError::Rejected(REJECTION);
    let mut mountinfo = Vec::new();
    File::open("/proc/self/mountinfo")
        .and_then(|file| {
            file.take(MAX_MOUNTINFO_BYTES + 1)
                .read_to_end(&mut mountinfo)
        })
        .map_err(|_| rejected())?;
    if mountinfo.len() as u64 > MAX_MOUNTINFO_BYTES {
        return Err(rejected());
    }
    let path = backing_path(&mountinfo, mount_id).ok_or_else(rejected)?;
    // Do not recurse through filesystem admission: an overmounted or nested
    // eCryptfs source is not proof of its underlying filesystem.
    let lower = open_absolute_handle(&path).map_err(|_| rejected())?;
    if !lower.metadata().map_err(|_| rejected())?.is_dir() {
        return Err(rejected());
    }
    let lower = filesystem_stat(&lower).map_err(|_| rejected())?;
    if !backing_is_qualified(upper, &lower) {
        return Err(rejected());
    }
    Ok(())
}

fn backing_is_qualified(upper: &libc::statfs, lower: &libc::statfs) -> bool {
    // Linux ecryptfs_statfs forwards the lower f_fsid unchanged. Bind the
    // inspected path to that identity: a stale/replaced mount source pathname
    // on another filesystem must not grant admission. A zero ID proves nothing.
    let upper_id = filesystem_id(upper);
    upper_id != [0; size_of::<libc::fsid_t>()]
        && upper_id == filesystem_id(lower)
        && linux_filesystem_is_qualified(lower.f_type)
}

fn filesystem_id(filesystem: &libc::statfs) -> [u8; size_of::<libc::fsid_t>()] {
    let mut bytes = [0; size_of::<libc::fsid_t>()];
    // Linux fsid_t consists of two initialized integer fields, without padding.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&filesystem.f_fsid as *const libc::fsid_t).cast::<u8>(),
            bytes.as_mut_ptr(),
            bytes.len(),
        );
    }
    bytes
}

fn backing_path(mountinfo: &[u8], mount_id: u64) -> Option<PathBuf> {
    for line in mountinfo.split(|byte| *byte == b'\n') {
        let id = line.split(|byte| *byte == b' ').next()?;
        if std::str::from_utf8(id).ok()?.parse::<u64>().ok()? != mount_id {
            continue;
        }
        let separator = line.windows(3).position(|bytes| bytes == b" - ")?;
        let mut fields = line[separator + 3..].split(|byte| *byte == b' ');
        if fields.next()? != b"ecryptfs" {
            return None;
        }
        let mut source = fields.next()?;
        let mut path = Vec::new();
        while let Some((&byte, rest)) = source.split_first() {
            if byte == b'\\' {
                let escaped = match rest.get(..3)? {
                    b"040" => b' ',
                    b"011" => b'\t',
                    b"012" => b'\n',
                    b"134" => b'\\',
                    _ => return None,
                };
                path.push(escaped);
                source = &rest[3..];
            } else {
                path.push(byte);
                source = rest;
            }
        }
        return Some(PathBuf::from(std::ffi::OsString::from_vec(path)));
    }
    None
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use super::*;

    #[test]
    fn backing_path_uses_exact_mount_id_and_decodes_kernel_escapes() {
        let records = b"12 1 0:1 / /upper rw - ecryptfs /wrong rw\n13 1 0:2 / /upper rw shared:1 - ecryptfs /lower\\040dir/\\134\\011\\012\xff rw\n";
        assert_eq!(
            backing_path(records, 13),
            Some(PathBuf::from(std::ffi::OsString::from_vec(
                b"/lower dir/\\\t\n\xff".to_vec()
            )))
        );
        for record in [
            b"13 1 0:2 / /upper rw - xfs /lower rw".as_slice(),
            b"13 1 0:2 / /upper rw - ecryptfs /lower\\04 rw",
            b"13 1 0:2 / /upper rw - ecryptfs /lower\\999 rw",
            b"12 1 0:2 / /upper rw - ecryptfs /lower rw",
        ] {
            assert_eq!(backing_path(record, 13), None);
        }
    }

    #[test]
    fn backing_must_have_a_matching_nonzero_id_and_qualified_type() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let mut lower = filesystem_stat(&File::open(temp.path()).unwrap()).unwrap();
        let upper = filesystem_stat(&File::open(temp.path()).unwrap()).unwrap();
        for filesystem in [0xEF53, 0x5846_5342, 0x9123_683E, 0xF2F5_2010, 0x0102_1994] {
            lower.f_type = filesystem;
            assert!(backing_is_qualified(&upper, &lower));
        }
        for filesystem in [
            0xF15F,
            0x4d44,
            0x6969,
            0xFF53_4D42,
            0x6573_5546,
            0x2FC1_2FC1,
            0,
        ] {
            lower.f_type = filesystem;
            assert!(!backing_is_qualified(&upper, &lower));
        }
        lower.f_type = 0xEF53;
        // Zero is also a different identity from the real test filesystem.
        lower.f_fsid = unsafe { std::mem::zeroed() };
        assert!(!backing_is_qualified(&upper, &lower));
        assert!(!backing_is_qualified(&lower, &lower));
    }
}
