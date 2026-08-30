use super::*;

#[test]
fn initial_legacy_directory_remains_compatible_without_owner_adoption(
) -> Result<(), Box<dyn std::error::Error>> {
    let parent = tempfile::tempdir()?;
    let path = parent.path().join("legacy-private-root");
    create_private_directory_all(&path)?;
    let before = OpenedPrivateObject::open(&path, ObjectKind::Directory, false)?;

    with_handle_owner(before.file(), |before_owner| {
        create_current_user_owned_private_directory_all(&path)?;
        let after = OpenedPrivateObject::open(&path, ObjectKind::Directory, false)?;
        with_handle_owner(after.file(), |after_owner| {
            // SAFETY: both SIDs remain backed by live security descriptors.
            if unsafe { EqualSid(before_owner, after_owner) } != 0 {
                Ok(())
            } else {
                Err(invalid_owner())
            }
        })
    })?;
    Ok(())
}

#[test]
fn current_user_owned_directory_rejects_a_wrong_owner_create_race(
) -> Result<(), Box<dyn std::error::Error>> {
    use windows_sys::Win32::Security::{TokenOwner, TOKEN_OWNER};

    let identities = PrivateIdentities::current()?;
    let token_owner = token_information(identities._token.0, TokenOwner)?;
    // SAFETY: token_owner contains a successful TOKEN_OWNER response.
    let token_owner_sid = unsafe { (*token_owner.as_ptr().cast::<TOKEN_OWNER>()).Owner };
    // A standard non-elevated token commonly defaults ownership to its user.
    // The governed elevated lane requires the distinct Administrators owner
    // needed to exercise the ERROR_ALREADY_EXISTS rejection deterministically.
    if unsafe { EqualSid(token_owner_sid, identities.user_sid()) } != 0 {
        assert_ne!(
            std::env::var("CTX_TEST_WINDOWS_ELEVATED_OWNER").as_deref(),
            Ok("1"),
            "the elevated native lane requires a distinct token-default owner"
        );
        return Ok(());
    }

    let parent = tempfile::tempdir()?;
    let path = parent.path().join("raced-private-root");
    let error = create_private_directory_all_with_owner_after_missing(&path, true, |candidate| {
        create_private_directory_all(candidate)
    })
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let raced = OpenedPrivateObject::open(&path, ObjectKind::Directory, false)?;
    verify_handle_with_identities(raced.file(), ObjectKind::Directory, &identities)?;
    with_handle_owner(raced.file(), |owner| {
        // SAFETY: both SIDs remain backed by live buffers.
        if unsafe { EqualSid(owner, token_owner_sid) } != 0
            && unsafe { EqualSid(owner, identities.user_sid()) } == 0
        {
            Ok(())
        } else {
            Err(invalid_owner())
        }
    })?;
    Ok(())
}
