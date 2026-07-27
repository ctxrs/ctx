use std::fs;

#[cfg(target_os = "windows")]
use std::{
    io::ErrorKind,
    os::windows::fs::{symlink_dir, symlink_file},
    path::Path,
};

use super::{
    collect_jsonl_paths_bounded, ensure_inventory_path_bound, inventory_provider_jsonl_paths,
    inventory_provider_regular_paths, provider_regular_file_len, ProviderJsonlInventoryLimits,
    PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use crate::{CaptureError, ProviderJsonlInventoryLimit};

#[cfg(target_os = "windows")]
use super::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    ensure_supported_windows_provider_path_prefix,
};

#[cfg(target_os = "windows")]
fn symlink_unavailable(error: &std::io::Error) -> bool {
    error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
}

#[test]
fn bounded_jsonl_collection_stops_before_allocating_the_max_plus_one_path() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for index in 0..4 {
        fs::write(temp.path().join(format!("{index}.jsonl")), b"{}\n").unwrap();
    }
    let mut paths = Vec::new();

    let error = collect_jsonl_paths_bounded(temp.path(), &mut paths, 3).unwrap_err();

    assert!(paths.is_empty());
    assert!(matches!(
        error,
        CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::EligiblePaths,
            maximum: 3,
            observed: 4,
        }
    ));
}

#[test]
fn non_jsonl_entries_consume_the_metadata_budget() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for index in 0..4 {
        fs::write(temp.path().join(format!("{index}.txt")), b"x").unwrap();
    }

    let error = inventory_provider_jsonl_paths(
        temp.path(),
        ProviderJsonlInventoryLimits {
            max_metadata_entries: 4,
            ..ProviderJsonlInventoryLimits::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::MetadataEntries,
            maximum: 4,
            observed: 5,
        }
    ));
}

#[test]
fn iterative_provider_inventory_rejects_depth_beyond_the_explicit_bound() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let mut directory = temp.path().to_path_buf();
    for index in 0..5 {
        directory.push(format!("d{index}"));
        fs::create_dir(&directory).unwrap();
    }

    let error = inventory_provider_jsonl_paths(
        temp.path(),
        ProviderJsonlInventoryLimits {
            max_depth: 3,
            ..ProviderJsonlInventoryLimits::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::Depth,
            maximum: 3,
            observed: 4,
        }
    ));
}

#[test]
fn wide_provider_inventory_rejects_too_many_directories() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for index in 0..4 {
        fs::create_dir(temp.path().join(format!("d{index}"))).unwrap();
    }

    let error = inventory_provider_jsonl_paths(
        temp.path(),
        ProviderJsonlInventoryLimits {
            max_directories: 3,
            ..ProviderJsonlInventoryLimits::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::Directories,
            maximum: 3,
            observed: 4,
        }
    ));
}

#[test]
fn provider_inventory_is_sorted_and_reports_only_admitted_jsonl_paths() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(temp.path().join("z.jsonl"), b"z").unwrap();
    fs::write(nested.join("a.jsonl"), b"a").unwrap();
    fs::write(temp.path().join("ignored.txt"), b"ignored").unwrap();

    let first =
        inventory_provider_jsonl_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap();
    let second =
        inventory_provider_jsonl_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap();

    assert_eq!(
        first.paths(),
        &[nested.join("a.jsonl"), temp.path().join("z.jsonl")]
    );
    assert_eq!(first, second);
    assert_eq!(first.directories(), 2);
    assert_eq!(first.metadata_entries(), 5);
}

#[test]
fn regular_provider_inventory_is_format_neutral_while_jsonl_inventory_is_narrow() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::write(temp.path().join("session.jsonl"), b"jsonl").unwrap();
    fs::write(temp.path().join("session.json"), b"json").unwrap();
    fs::write(temp.path().join("state.db"), b"db").unwrap();
    fs::write(temp.path().join("state.sqlite"), b"sqlite").unwrap();
    fs::write(temp.path().join("state.vscdb"), b"vscdb").unwrap();
    fs::write(temp.path().join("opaque"), b"opaque").unwrap();

    let jsonl =
        inventory_provider_jsonl_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap();
    let regular =
        inventory_provider_regular_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap();

    assert_eq!(jsonl.paths(), &[temp.path().join("session.jsonl")]);
    assert_eq!(
        regular.paths(),
        &[
            temp.path().join("opaque"),
            temp.path().join("session.json"),
            temp.path().join("session.jsonl"),
            temp.path().join("state.db"),
            temp.path().join("state.sqlite"),
            temp.path().join("state.vscdb"),
        ]
    );
}

#[test]
fn regular_provider_inventory_applies_file_and_metadata_limits_to_non_jsonl_sources() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::write(temp.path().join("session.json"), b"json").unwrap();
    fs::write(temp.path().join("state.sqlite"), b"sqlite").unwrap();

    let exact = inventory_provider_regular_paths(
        temp.path(),
        ProviderJsonlInventoryLimits {
            max_directories: 1,
            max_depth: 0,
            max_eligible_paths: 2,
            max_metadata_entries: 3,
        },
    )
    .unwrap();
    assert_eq!(exact.paths().len(), 2);
    assert_eq!(exact.directories(), 1);
    assert_eq!(exact.metadata_entries(), 3);

    fs::write(temp.path().join("state.vscdb"), b"vscdb").unwrap();
    let error = inventory_provider_regular_paths(
        temp.path(),
        ProviderJsonlInventoryLimits {
            max_eligible_paths: 2,
            ..ProviderJsonlInventoryLimits::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::EligiblePaths,
            maximum: 2,
            observed: 3,
        }
    ));
}

#[cfg(unix)]
#[test]
fn provider_inventory_rejects_symlinked_tree_entries_without_following_them() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let outside = crate::test_support_paths::tempdir().unwrap();
    fs::write(outside.path().join("session.jsonl"), b"{}\n").unwrap();
    symlink(outside.path(), temp.path().join("linked")).unwrap();

    let error =
        inventory_provider_jsonl_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { .. }
    ));

    let error =
        inventory_provider_regular_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap_err();
    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { .. }
    ));
}

#[cfg(unix)]
#[test]
fn provider_inventory_rejects_nonregular_jsonl_entries() {
    use std::os::unix::net::UnixListener;

    let temp = tempfile::Builder::new()
        .prefix("ctx-io-")
        .tempdir_in("/tmp")
        .unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let _listener = UnixListener::bind(root.join("socket.jsonl")).unwrap();

    let error =
        inventory_provider_jsonl_paths(&root, ProviderJsonlInventoryLimits::default()).unwrap_err();

    assert!(matches!(
        error,
        CaptureError::InvalidProviderTranscriptPath { .. }
    ));
}

#[test]
fn provider_inventory_rejects_overlong_encoded_paths_before_io() {
    let path = std::path::PathBuf::from("x".repeat(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES + 1));

    let error = ensure_inventory_path_bound(&path).unwrap_err();
    assert!(error.to_string().contains("provider source path exceeds"));

    let error =
        inventory_provider_jsonl_paths(&path, ProviderJsonlInventoryLimits::default()).unwrap_err();
    assert!(matches!(error, CaptureError::InvalidPayload(_)));
}

#[test]
fn regular_file_length_is_accounted_without_weakening_path_validation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("state.sqlite-shm");
    fs::write(&path, b"volatile").unwrap();

    assert_eq!(provider_regular_file_len(&path).unwrap(), 8);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_ordinary_absolute_provider_file_is_accepted() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("provider.db");
    fs::write(&path, b"provider").unwrap();

    assert!(path.is_absolute());
    ensure_regular_provider_transcript_file(&path).unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn windows_supported_rooted_prefixes_are_accepted_without_io() {
    for path in [
        Path::new(r"C:\provider.db"),
        Path::new(r"\\?\C:\provider.db"),
        Path::new(r"\\server\share\provider.db"),
        Path::new(r"\\?\UNC\server\share\provider.db"),
    ] {
        ensure_supported_windows_provider_path_prefix(path).unwrap();
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_drive_relative_provider_path_is_rejected() {
    assert!(
        ensure_provider_path_parents_are_not_symlinks(Path::new(r"C:provider\history.jsonl"))
            .is_err()
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reparse_file_is_rejected() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let target = temp.path().join("target.db");
    let link = temp.path().join("link.db");
    fs::write(&target, b"provider").unwrap();
    if let Err(error) = symlink_file(&target, &link) {
        if symlink_unavailable(&error) {
            return;
        }
        panic!("failed to create Windows file symlink: {error}");
    }

    assert!(ensure_regular_provider_transcript_file(&link).is_err());
    assert!(provider_regular_file_len(&link).is_err());
    assert!(
        inventory_provider_regular_paths(&link, ProviderJsonlInventoryLimits::default()).is_err()
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_reparse_parent_is_rejected() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let target = temp.path().join("target");
    let link = temp.path().join("link");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("provider.db"), b"provider").unwrap();
    if let Err(error) = symlink_dir(&target, &link) {
        if symlink_unavailable(&error) {
            return;
        }
        panic!("failed to create Windows directory symlink: {error}");
    }

    assert!(ensure_provider_path_parents_are_not_symlinks(&link.join("provider.db")).is_err());
}
