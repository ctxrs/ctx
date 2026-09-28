use super::*;

const VERSIONS: [RetainedFileIdentityVersion; 2] = [
    RetainedFileIdentityVersion::SharedJsonlV1,
    RetainedFileIdentityVersion::OrdinaryFileV2,
];

#[test]
fn reboot_changes_device_number_but_not_either_retained_identity() {
    let file = tempfile::tempfile().unwrap();
    let metadata = file.metadata().unwrap();
    for version in VERSIONS {
        let before = unix_tokens(&metadata, version, 100, Some([0x31; 16]));
        let after = unix_tokens(&metadata, version, 900, Some([0x31; 16]));
        assert_eq!(
            before, after,
            "unchanged files must remain on the no-op path"
        );
        assert_ne!(
            before.0,
            unix_tokens(&metadata, version, 100, None).0,
            "old device-based checkpoints are replaced once"
        );
    }
}

#[test]
fn reused_device_number_on_another_volume_or_replaced_inode_is_not_the_same_file() {
    let original = tempfile::tempfile().unwrap();
    let replacement = tempfile::tempfile().unwrap();
    let metadata = original.metadata().unwrap();
    let replacement_metadata = replacement.metadata().unwrap();
    assert_ne!(metadata.ino(), replacement_metadata.ino());
    for version in VERSIONS {
        let before = unix_tokens(&metadata, version, 100, Some([0x31; 16]));
        assert_ne!(
            before.0,
            unix_tokens(&metadata, version, 100, Some([0x32; 16])).0
        );
        assert_ne!(
            before.0,
            unix_tokens(&replacement_metadata, version, 100, Some([0x31; 16])).0
        );
    }
}

#[test]
fn unsupported_volume_uuid_preserves_device_identity_without_hiding_io_errors() {
    let file = tempfile::tempfile().unwrap();
    let metadata = file.metadata().unwrap();
    for errno in [libc::EINVAL, libc::ENOTSUP] {
        let uuid = decode_volume_uuid(Err(io::Error::from_raw_os_error(errno)), reply(20)).unwrap();
        assert_eq!(uuid, None);
        for version in VERSIONS {
            assert_ne!(
                unix_tokens(&metadata, version, 100, uuid).0,
                unix_tokens(&metadata, version, 900, uuid).0
            );
        }
    }
    for errno in [libc::EIO, libc::EINTR, libc::EBADF, libc::EACCES] {
        let error =
            decode_volume_uuid(Err(io::Error::from_raw_os_error(errno)), reply(20)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(errno));
    }
}

#[test]
fn volume_uuid_reply_requires_the_complete_attribute() {
    assert_eq!(std::mem::size_of::<VolumeUuidReply>(), 20);
    assert_eq!(
        decode_volume_uuid(Ok(()), reply(20)).unwrap(),
        Some([0x31; 16])
    );
    for length in [0, 4, 19, 21, u32::MAX] {
        assert_eq!(
            decode_volume_uuid(Ok(()), reply(length))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}

fn reply(length: u32) -> VolumeUuidReply {
    VolumeUuidReply {
        length,
        uuid: [0x31; 16],
    }
}

#[cfg(target_os = "macos")]
#[test]
fn native_macos_volume_identity_follows_the_open_file_after_rename() {
    use std::os::fd::AsRawFd;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("history.jsonl");
    std::fs::write(&path, b"original\n").unwrap();
    let file = File::open(&path).unwrap();
    let uuid = boot_stable_volume_uuid(&file).unwrap();
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: the descriptor is open and the buffer is correctly sized.
    assert_eq!(
        unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) },
        0
    );
    // SAFETY: successful fstatfs initialized the buffer and its terminated name.
    let filesystem = unsafe { filesystem.assume_init() };
    let filesystem_type = unsafe { std::ffi::CStr::from_ptr(filesystem.f_fstypename.as_ptr()) };
    if matches!(filesystem_type.to_bytes(), b"apfs" | b"hfs") {
        assert!(
            uuid.is_some(),
            "native local volumes must supply their persistent UUID"
        );
    }
    std::fs::rename(&path, temp.path().join("renamed.jsonl")).unwrap();
    std::fs::write(&path, b"replacement\n").unwrap();
    assert_eq!(boot_stable_volume_uuid(&file).unwrap(), uuid);
    let replacement = File::open(&path).unwrap();
    for version in VERSIONS {
        let original_identity =
            retained_file_identity(&file, &file.metadata().unwrap(), version).unwrap();
        assert_ne!(
            original_identity.0,
            retained_file_identity(&replacement, &replacement.metadata().unwrap(), version)
                .unwrap()
                .0
        );
    }
}
