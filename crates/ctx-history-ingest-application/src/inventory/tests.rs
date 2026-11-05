use super::*;

#[test]
fn file_accounting_does_not_contain_systemic_or_source_change_errors() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("session.jsonl");
    fs::write(&file, b"{}\n").unwrap();
    for error in [
        SourceIoError::SystemInvariant("test invariant"),
        SourceIoError::SourceChangedDuringCapture,
        SourceIoError::Io(std::io::ErrorKind::OutOfMemory.into()),
        SourceIoError::SystemIo {
            operation: "ctx-owned publication",
            source: std::io::ErrorKind::PermissionDenied.into(),
        },
    ] {
        let mut stats = SourceStats::default();
        assert!(account_unreadable_file(&mut stats, &file, error).is_err());
        assert_eq!(stats, SourceStats::default());
    }
}

#[test]
fn source_accounting_keeps_inventory_limits_fatal() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), b"{}\n").unwrap();
    let error = source_stats_with_inventory_limits(
        temp.path(),
        ProviderJsonlInventoryLimits {
            max_metadata_entries: 1,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SourceIoError>(),
        Some(SourceIoError::ProviderJsonlInventoryLimitExceeded { .. })
    ));
}

#[cfg(unix)]
#[test]
fn unreadable_file_accounting_preserves_totals_without_a_partial_token() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("sidecar.jsonl");
    fs::write(&file, b"sidecar\n").unwrap();
    fs::write(temp.path().join("session.jsonl"), b"{}\n").unwrap();
    let before = source_stats(temp.path()).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    let denied = fs::File::open(&file);
    let result = source_stats(temp.path());
    let exact_file = source_stats(&file);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    if denied.is_ok() {
        return;
    }
    assert_eq!(
        denied.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let partial = result.unwrap();
    assert_eq!((partial.files, partial.bytes), (2, 11));
    assert_eq!((partial.files, partial.bytes), (before.files, before.bytes));
    assert!(partial.change_token.is_none());
    assert!(
        exact_file.is_err(),
        "an unreadable explicit root stays fatal"
    );
    assert!(source_stats(temp.path()).unwrap().change_token.is_some());
}
