use super::*;

fn verified_ntfs() -> FilesystemIdentity {
    FilesystemIdentity {
        volume_serial_number: 1,
        filesystem_name: "NTFS".to_owned(),
    }
}

#[test]
fn explicit_non_cloud_and_unsupported_verified_ntfs_results_are_accepted() {
    let filesystem = verified_ntfs();
    assert!(
        qualify_sync_root_query(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT_HRESULT, &filesystem,).is_ok()
    );
    assert!(qualify_sync_root_query(ERROR_INVALID_FUNCTION_HRESULT, &filesystem).is_ok());
}

#[test]
fn unsupported_cloud_query_requires_verified_ntfs() {
    let unqualified = FilesystemIdentity {
        volume_serial_number: 1,
        filesystem_name: "ReFS".to_owned(),
    };
    assert!(matches!(
        qualify_sync_root_query(ERROR_INVALID_FUNCTION_HRESULT, &unqualified),
        Err(AuthorityOpenError::Rejected(
            "Windows could not qualify the provider source as non-cloud storage"
        ))
    ));
}

#[test]
fn not_a_cloud_placeholder_does_not_prove_not_under_sync_root() {
    let filesystem = verified_ntfs();

    let not_a_cloud_file =
        win32_error_hresult(windows_sys::Win32::Foundation::ERROR_NOT_A_CLOUD_FILE);
    assert!(matches!(
        qualify_sync_root_query(not_a_cloud_file, &filesystem),
        Err(AuthorityOpenError::Rejected(
            "Windows could not qualify the provider source as non-cloud storage"
        ))
    ));
}

#[test]
fn sync_root_and_ambiguous_query_results_remain_rejected() {
    let filesystem = verified_ntfs();
    let access_denied = win32_error_hresult(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED);
    assert!(matches!(
        qualify_sync_root_query(0, &filesystem),
        Err(AuthorityOpenError::Rejected(
            "cloud-synchronized provider source roots are rejected"
        ))
    ));
    assert!(matches!(
        qualify_sync_root_query(access_denied, &filesystem),
        Err(AuthorityOpenError::Rejected(
            "Windows could not qualify the provider source as non-cloud storage"
        ))
    ));
}
