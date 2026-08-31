use super::*;

fn assert_token_default_owner(handle: &File, identities: &PrivateIdentities) -> io::Result<()> {
    use windows_sys::Win32::Security::{TokenOwner, TOKEN_OWNER};

    let token_owner = token_information(identities._token.0, TokenOwner)?;
    // SAFETY: token_owner contains a successful TOKEN_OWNER response.
    let token_owner_sid = unsafe { (*token_owner.as_ptr().cast::<TOKEN_OWNER>()).Owner };
    with_handle_owner(handle, |owner| {
        // SAFETY: all compared SIDs remain backed by live buffers.
        assert_ne!(unsafe { EqualSid(owner, token_owner_sid) }, 0);
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
