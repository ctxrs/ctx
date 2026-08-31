use std::fs;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
))]
use std::io::Read;

#[cfg(target_os = "windows")]
use std::{
    io::ErrorKind,
    os::windows::fs::{symlink_dir, symlink_file},
    path::{Path, PathBuf},
};

use super::{
    collect_jsonl_paths_bounded, ensure_inventory_path_bound, inventory_provider_jsonl_paths,
    inventory_provider_regular_paths, open_provider_source_file, provider_regular_file_len,
    ProviderJsonlInventoryLimits, ProviderSourceRoot, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use crate::{ProviderJsonlInventoryLimit, SourceIoError};

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
        SourceIoError::ProviderJsonlInventoryLimitExceeded {
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
        SourceIoError::ProviderJsonlInventoryLimitExceeded {
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
        SourceIoError::ProviderJsonlInventoryLimitExceeded {
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
        SourceIoError::ProviderJsonlInventoryLimitExceeded {
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
        SourceIoError::ProviderJsonlInventoryLimitExceeded {
            limit: ProviderJsonlInventoryLimit::EligiblePaths,
            maximum: 2,
            observed: 3,
        }
    ));
}

#[cfg(unix)]
#[test]
fn provider_inventory_skips_symlinked_tree_entries_without_following_them() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let outside = crate::test_support_paths::tempdir().unwrap();
    fs::write(outside.path().join("session.jsonl"), b"{}\n").unwrap();
    symlink(outside.path(), temp.path().join("linked")).unwrap();
    fs::write(temp.path().join("local.jsonl"), b"{}\n").unwrap();

    // The symlinked directory is skipped, never followed: the outside
    // `session.jsonl` must not leak into the inventory, while the regular
    // sibling is still admitted.
    let inventory =
        inventory_provider_jsonl_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap();
    assert_eq!(inventory.paths(), &[temp.path().join("local.jsonl")]);

    let inventory =
        inventory_provider_regular_paths(temp.path(), ProviderJsonlInventoryLimits::default())
            .unwrap();
    assert_eq!(inventory.paths(), &[temp.path().join("local.jsonl")]);
}

#[cfg(unix)]
#[test]
fn provider_inventory_skips_nonregular_jsonl_entries() {
    use crate::test_support_paths::make_fifo;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    make_fifo(&root.join("fifo.jsonl")).unwrap();
    fs::write(root.join("session.jsonl"), b"{}\n").unwrap();

    let inventory =
        inventory_provider_jsonl_paths(&root, ProviderJsonlInventoryLimits::default()).unwrap();

    assert_eq!(inventory.paths(), &[root.join("session.jsonl")]);
}

#[test]
fn provider_inventory_rejects_overlong_encoded_paths_before_io() {
    let path = std::path::PathBuf::from("x".repeat(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES + 1));

    let error = ensure_inventory_path_bound(&path).unwrap_err();
    assert!(error.to_string().contains("provider source path exceeds"));

    let error =
        inventory_provider_jsonl_paths(&path, ProviderJsonlInventoryLimits::default()).unwrap_err();
    assert!(matches!(error, SourceIoError::InvalidPayload(_)));
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
    let source = open_provider_source_file(&path).unwrap();
    assert_eq!(source.read_all_bounded(8).unwrap(), b"provider");
    source.revalidate().unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn windows_qualified_external_provider_file_is_accepted() {
    let mut retained_temp = None;
    let path = match std::env::var_os("CTX_TEST_WINDOWS_PROVIDER_FILE") {
        Some(path) => PathBuf::from(path),
        None => {
            let temp = crate::test_support_paths::tempdir().unwrap();
            let path = temp.path().join("provider-external.db");
            fs::write(&path, b"provider").unwrap();
            retained_temp = Some(temp);
            path
        }
    };
    let expected = fs::read(&path).unwrap();

    let source = open_provider_source_file(&path).unwrap();
    assert_eq!(source.read_all_bounded(1024 * 1024).unwrap(), expected);
    source.revalidate().unwrap();
    drop(retained_temp);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_local_rooted_prefixes_are_accepted_without_io() {
    for path in [
        Path::new(r"C:\provider.db"),
        Path::new(r"\\?\C:\provider.db"),
    ] {
        ensure_supported_windows_provider_path_prefix(path).unwrap();
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_network_roots_are_rejected_without_io() {
    for path in [
        Path::new(r"\\server\share\provider.db"),
        Path::new(r"\\?\UNC\server\share\provider.db"),
    ] {
        assert!(ensure_supported_windows_provider_path_prefix(path).is_err());
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
fn source_root_safety_windows_reparse_file_is_rejected() {
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
fn source_root_safety_windows_reparse_parent_is_rejected() {
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

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
))]
fn assert_retained_authority_changed(error: SourceIoError) {
    assert!(matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath { reason, .. }
            if reason.contains("changed while its authority handle was retained")
    ));
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
))]
fn replace_directory_from_thread(
    named: std::path::PathBuf,
    moved: std::path::PathBuf,
    replacement: std::path::PathBuf,
) {
    std::thread::spawn(move || {
        fs::rename(&named, moved).unwrap();
        fs::rename(replacement, named).unwrap();
    })
    .join()
    .unwrap();
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
))]
#[test]
fn source_root_safety_retained_root_reads_exact_original_after_named_root_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let moved = temp.path().join("moved-root");
    let replacement = temp.path().join("replacement-root");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(root.join("source.jsonl"), b"inside-root\n").unwrap();
    fs::write(
        replacement.join("source.jsonl"),
        b"OUTSIDE_ROOT_MUST_NOT_ESCAPE\n",
    )
    .unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();

    replace_directory_from_thread(root.clone(), moved, replacement);

    let source = authority
        .open_file(std::path::Path::new("source.jsonl"))
        .unwrap();
    let mut bytes = Vec::new();
    source
        .bounded_reader(64)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"inside-root\n");
    assert!(!bytes
        .windows(b"OUTSIDE_ROOT_MUST_NOT_ESCAPE".len())
        .any(|window| window == b"OUTSIDE_ROOT_MUST_NOT_ESCAPE"));
    assert_retained_authority_changed(authority.revalidate().unwrap_err());
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
))]
#[test]
fn source_root_safety_retained_root_reads_exact_original_after_ancestor_swap() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let parent = temp.path().join("parent");
    let moved_parent = temp.path().join("moved-parent");
    let root = parent.join("root");
    let replacement_parent = temp.path().join("replacement-parent");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(replacement_parent.join("root")).unwrap();
    fs::write(root.join("source.jsonl"), b"inside-ancestor\n").unwrap();
    fs::write(
        replacement_parent.join("root/source.jsonl"),
        b"OUTSIDE_ANCESTOR_MUST_NOT_ESCAPE\n",
    )
    .unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();

    replace_directory_from_thread(parent, moved_parent, replacement_parent);

    let source = authority
        .open_file(std::path::Path::new("source.jsonl"))
        .unwrap();
    let mut bytes = Vec::new();
    source
        .bounded_reader(64)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"inside-ancestor\n");
    assert_retained_authority_changed(authority.revalidate().unwrap_err());
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
))]
#[test]
fn source_root_safety_retained_leaf_reads_exact_original_then_revalidation_fails() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    let moved = temp.path().join("moved-source.jsonl");
    let replacement = temp.path().join("replacement.jsonl");
    fs::write(&path, b"inside-leaf\n").unwrap();
    fs::write(&replacement, b"OUTSIDE_LEAF\n").unwrap();
    let source = open_provider_source_file(&path).unwrap();

    std::thread::spawn({
        let path = path.clone();
        move || {
            fs::rename(&path, moved).unwrap();
            fs::rename(replacement, path).unwrap();
        }
    })
    .join()
    .unwrap();

    let mut bytes = Vec::new();
    source
        .bounded_reader(64)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"inside-leaf\n");
    assert_retained_authority_changed(source.revalidate().unwrap_err());
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
#[test]
fn source_root_safety_concurrent_descendant_symlink_swap_cannot_read_outside_root() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let outside = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let nested = root.join("nested");
    let moved = root.join("moved-nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("source.jsonl"), b"inside\n").unwrap();
    fs::write(
        outside.path().join("source.jsonl"),
        b"OUTSIDE_SYMLINK_MUST_NOT_ESCAPE\n",
    )
    .unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();
    let outside_path = outside.path().to_path_buf();

    std::thread::spawn(move || {
        fs::rename(nested, moved).unwrap();
        symlink(outside_path, root.join("nested")).unwrap();
    })
    .join()
    .unwrap();

    assert!(matches!(
        authority.open_file(std::path::Path::new("nested/source.jsonl")),
        Err(SourceIoError::InvalidProviderTranscriptPath { .. })
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn source_root_safety_linux_virtual_and_unqualified_roots_fail_closed() {
    for path in [std::path::Path::new("/proc"), std::path::Path::new("/sys")] {
        assert!(matches!(
            ProviderSourceRoot::open(path),
            Err(SourceIoError::InvalidProviderTranscriptPath { .. })
        ));
    }
}

#[cfg(target_os = "windows")]
#[test]
fn source_root_safety_windows_unc_network_and_device_roots_fail_closed() {
    for path in [
        Path::new(r"\\server\share"),
        Path::new(r"\\?\UNC\server\share"),
        Path::new(r"\\.\C:\provider"),
    ] {
        assert!(matches!(
            ProviderSourceRoot::open(path),
            Err(SourceIoError::InvalidProviderTranscriptPath { .. })
        ));
    }
}

#[cfg(target_os = "windows")]
#[test]
fn source_root_safety_windows_cloud_and_reparse_policy_remains_fail_closed() {
    let policy = include_str!("io/root_handle/windows.rs");
    for required in [
        "FILE_ATTRIBUTE_OFFLINE",
        "FILE_ATTRIBUTE_RECALL_ON_OPEN",
        "FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS",
        "CfGetSyncRootInfoByHandle",
        "cloud-synchronized provider source roots are rejected",
    ] {
        assert!(
            policy.contains(required),
            "Windows authority policy lost {required}"
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
#[test]
fn source_root_safety_bsd_family_local_authority_fixture_reads_exact_bytes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    fs::write(&path, b"local-authority\n").unwrap();

    let source = open_provider_source_file(&path).unwrap();

    assert_eq!(source.read_all_bounded(64).unwrap(), b"local-authority\n");
    source.revalidate().unwrap();
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "windows"
)))]
#[test]
fn source_root_safety_unsupported_platform_fails_closed() {
    assert!(matches!(
        ProviderSourceRoot::open(std::path::Path::new("/provider")),
        Err(SourceIoError::InvalidProviderTranscriptPath { .. })
    ));
}
