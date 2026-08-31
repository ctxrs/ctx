use super::*;

fn assert_token_default_owner(handle: &File, identities: &PrivateIdentities) -> io::Result<()> {
    with_handle_owner(handle, |owner| {
        // SAFETY: all compared SIDs remain backed by live buffers.
        assert_ne!(unsafe { EqualSid(owner, identities.token_owner_sid()) }, 0);
        if std::env::var("CTX_TEST_WINDOWS_ELEVATED_OWNER").as_deref() == Ok("1") {
            assert_eq!(
                unsafe { EqualSid(owner, identities.user_sid()) },
                0,
                "the elevated lane requires a distinct token-default owner"
            );
        }
        Ok(())
    })
}

#[test]
fn generic_private_objects_reject_an_owner_outside_token_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    use windows_sys::Win32::Security::WinWorldSid;

    let identities = PrivateIdentities::current()?;
    let mut world = AlignedBuffer::new(SECURITY_MAX_SID_SIZE)?;
    let mut world_size = u32::try_from(world.byte_len()).map_err(|_| invalid_owner())?;
    // SAFETY: world is aligned and has SECURITY_MAX_SID_SIZE capacity.
    if unsafe {
        CreateWellKnownSid(
            WinWorldSid,
            null_mut(),
            world.as_mut_ptr().cast(),
            &raw mut world_size,
        )
    } == 0
    {
        return Err(last_error().into());
    }
    let world_sid = world.as_ptr().cast_mut().cast();
    // SAFETY: all SIDs remain backed by live buffers.
    assert_eq!(unsafe { EqualSid(world_sid, identities.user_sid()) }, 0);
    // SAFETY: all SIDs remain backed by live buffers.
    assert_eq!(
        unsafe { EqualSid(world_sid, identities.token_owner_sid()) },
        0
    );

    let error = verify_admissible_owner(world_sid, &identities).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        error.to_string(),
        "private state path owner is outside the current token authority"
    );
    Ok(())
}

#[test]
fn dacl_restriction_preserves_the_existing_owner() -> Result<(), Box<dyn std::error::Error>> {
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("legacy-default-owner.lock");
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(&path)?;

    with_handle_owner(&file, |before| {
        restrict_private_file_handle(&file)?;
        with_handle_owner(&file, |after| {
            // SAFETY: both SIDs remain backed by live security descriptors.
            if unsafe { EqualSid(before, after) } != 0 {
                Ok(())
            } else {
                Err(invalid_owner())
            }
        })
    })?;

    let identities = PrivateIdentities::current()?;
    assert_token_default_owner(&file, &identities)?;
    verify_handle_with_identities(&file, ObjectKind::File, &identities)?;
    verify_handle(&file, ObjectKind::File)?;
    Ok(())
}

#[test]
fn generic_private_objects_accept_a_distinct_token_default_owner(
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = tempfile::tempdir()?;
    let directory = parent.path().join("legacy-default-owner-directory");
    create_private_directory_all(&directory)?;
    let directory = OpenedPrivateObject::open(&directory, ObjectKind::Directory, false)?;
    let identities = PrivateIdentities::current()?;

    assert_token_default_owner(directory.file(), &identities)?;
    verify_handle(directory.file(), ObjectKind::Directory)?;

    let path = parent.path().join("legacy-default-owner-file");
    let mut options = fs::OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(&path)?;
    assert_token_default_owner(&file, &identities)?;

    ensure_private_file_handle(&file)?;

    assert_token_default_owner(&file, &identities)?;
    verify_handle(&file, ObjectKind::File)?;
    Ok(())
}
