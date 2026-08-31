use std::{fs, io::Read, time::Duration};

use super::*;

#[test]
fn provider_source_system_io_names_the_operation_and_path() {
    let path = Path::new("provider-root/history.jsonl");
    let error = map_open_error(
        path,
        AuthorityOpenError::SystemIo {
            operation: "provider source target open",
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        },
    );
    let rendered = error.to_string();

    assert!(matches!(
        error,
        SourceIoError::SystemIo {
            operation: "provider source target open",
            ..
        }
    ));
    assert!(rendered.contains("provider source target open"));
    assert!(rendered.contains("provider-root/history.jsonl"));
    assert!(rendered.contains("permission denied"));
}

#[test]
fn streaming_entries_stop_at_the_existing_bounded_entry_diagnostic() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    fs::create_dir(&root).unwrap();
    for index in 0..65 {
        fs::write(root.join(format!("entry-{index:02}")), b"entry").unwrap();
    }
    let directory = ProviderSourceRoot::open(&root)
        .unwrap()
        .directory()
        .unwrap();
    let mut visited = 0_usize;

    let error = directory
        .visit_entries(64, |_name| {
            visited += 1;
            Ok::<(), SourceIoError>(())
        })
        .unwrap_err();

    assert_eq!(visited, 64);
    assert!(matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath {
            reason: "provider source directory exceeds its bounded entry budget",
            ..
        }
    ));
}

#[test]
fn streaming_entries_use_retained_authority_and_preserve_visitor_errors() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let moved = temp.path().join("moved-root");
    let replacement = temp.path().join("replacement");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(root.join("original.jsonl"), b"original").unwrap();
    fs::write(replacement.join("replacement.jsonl"), b"replacement").unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();
    let directory = authority.directory().unwrap();

    fs::rename(&root, &moved).unwrap();
    fs::rename(&replacement, &root).unwrap();

    let mut names = Vec::new();
    directory
        .visit_entries(64, |name| {
            names.push(name);
            Ok::<(), SourceIoError>(())
        })
        .unwrap();
    assert_eq!(names, [OsString::from("original.jsonl")]);
    assert!(authority.revalidate().is_err());

    let error = directory
        .visit_entries(64, |_name| {
            Err(SourceIoError::InvalidPayload("visitor stopped".to_owned()))
        })
        .unwrap_err();
    assert!(matches!(
        error,
        SourceIoError::InvalidPayload(detail) if detail == "visitor stopped"
    ));
}

#[test]
fn retained_root_reads_the_original_tree_after_named_root_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let moved = temp.path().join("moved-root");
    let replacement = temp.path().join("replacement");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(root.join("source.jsonl"), b"original\n").unwrap();
    fs::write(replacement.join("source.jsonl"), b"replacement\n").unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();

    fs::rename(&root, &moved).unwrap();
    fs::rename(&replacement, &root).unwrap();

    let source = authority.open_file(Path::new("source.jsonl")).unwrap();
    let mut reader = source.bounded_reader(64).unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original\n");
    assert!(authority.revalidate().is_err());
}

#[cfg(unix)]
#[test]
fn replaced_descendant_symlink_cannot_escape_retained_root() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let outside = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let nested = root.join("nested");
    let moved = root.join("moved-nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("source.jsonl"), b"inside\n").unwrap();
    fs::write(outside.path().join("source.jsonl"), b"outside\n").unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();

    fs::rename(&nested, &moved).unwrap();
    symlink(outside.path(), &nested).unwrap();

    assert!(authority
        .open_file(Path::new("nested/source.jsonl"))
        .is_err());
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
#[test]
fn symlinked_ancestor_is_classified_as_a_rejected_provider_path() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let target = temp.path().join("target");
    let linked = temp.path().join("linked");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("source.jsonl"), b"inside\n").unwrap();
    symlink(&target, &linked).unwrap();

    let error = open_provider_source_file(&linked.join("source.jsonl")).unwrap_err();

    assert!(matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath { reason, .. }
            if reason.contains("symlinked provider source path components")
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
#[test]
fn plain_file_ancestor_preserves_the_raw_not_a_directory_io_error() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let plain_file = temp.path().join("plain-file");
    fs::write(&plain_file, b"ordinary").unwrap();

    let error = open_provider_source_file(&plain_file.join("source.jsonl")).unwrap_err();

    assert!(matches!(
        error,
        SourceIoError::Io(error) if error.raw_os_error() == Some(libc::ENOTDIR)
    ));
}

#[test]
fn descendants_reject_absolute_and_parent_escape() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let authority = ProviderSourceRoot::open(temp.path()).unwrap();

    assert!(authority.open_path(Path::new("../outside")).is_err());
    assert!(authority.open_path(temp.path()).is_err());
}

#[test]
fn exact_range_reads_from_open_handle_and_detects_named_replacement() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    let moved = temp.path().join("moved.jsonl");
    fs::write(&path, b"0123456789").unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    let source = root.open_file(Path::new("source.jsonl")).unwrap();

    assert_eq!(source.read_exact_range(3, 4, 4).unwrap(), b"3456");
    fs::rename(&path, &moved).unwrap();
    fs::write(&path, b"abcdefghij").unwrap();

    let mut retained = source.bounded_reader(10).unwrap();
    let mut bytes = Vec::new();
    retained.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"0123456789");
    assert!(source.revalidate_leaf().is_err());
    assert!(source.revalidate().is_err());
}

#[test]
fn active_source_family_contract_retained_handle_allows_growth_and_rejects_replacement() {
    use std::io::Write;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    let moved = temp.path().join("moved.jsonl");
    fs::write(&path, b"first\n").unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    let source = root.open_file(Path::new("source.jsonl")).unwrap();

    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"second\n")
        .unwrap();
    fs::write(temp.path().join("new-sibling.jsonl"), b"sibling\n").unwrap();
    assert_eq!(
        source.read_exact_range_allow_append(0, 6, 6).unwrap(),
        b"first\n"
    );
    assert!(source.revalidate_same_object().is_ok());
    assert!(root.revalidate_same_object().is_ok());
    assert!(source.revalidate().is_err());

    fs::rename(&path, &moved).unwrap();
    fs::write(&path, b"replacement\n").unwrap();
    assert!(source.revalidate_same_object_leaf().is_err());
    assert!(source.read_exact_range_allow_append(0, 6, 6).is_err());
}

#[test]
fn leaf_revalidation_and_one_terminal_root_fence_have_distinct_purposes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let moved = temp.path().join("moved-root");
    let replacement = temp.path().join("replacement");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(root.join("source.jsonl"), b"original\n").unwrap();
    fs::write(replacement.join("source.jsonl"), b"replacement\n").unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();
    let source = authority.open_file(Path::new("source.jsonl")).unwrap();

    fs::rename(&root, &moved).unwrap();
    fs::rename(&replacement, &root).unwrap();

    source.revalidate_leaf().unwrap();
    assert!(authority.revalidate_same_object().is_err());
    assert!(authority.revalidate().is_err());
    assert!(source.revalidate_same_object().is_err());
    assert!(source.revalidate().is_err());
}

#[test]
fn authority_fingerprints_are_stable_for_the_same_objects_and_change_on_mutation() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("root");
    let path = root.join("source.json");
    fs::create_dir(&root).unwrap();
    fs::write(&path, b"before").unwrap();

    let first_root = ProviderSourceRoot::open(&root).unwrap();
    let first_file = first_root.open_file(Path::new("source.json")).unwrap();
    let reopened_root = ProviderSourceRoot::open(&root).unwrap();
    let reopened_file = reopened_root.open_file(Path::new("source.json")).unwrap();
    assert_eq!(
        first_root.authority_fingerprint(),
        reopened_root.authority_fingerprint()
    );
    assert_eq!(
        first_file.authority_fingerprint(),
        reopened_file.authority_fingerprint()
    );

    let changed_modified = fs::metadata(&path)
        .unwrap()
        .modified()
        .unwrap()
        .checked_add(Duration::from_secs(2))
        .unwrap();
    fs::write(&path, b"after!").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(changed_modified))
        .unwrap();
    let changed_file = ProviderSourceRoot::open(&root)
        .unwrap()
        .open_file(Path::new("source.json"))
        .unwrap();
    assert_ne!(
        first_file.authority_fingerprint(),
        changed_file.authority_fingerprint()
    );
    assert!(first_file.revalidate().is_err());

    let moved_root = temp.path().join("moved-root");
    fs::rename(&root, &moved_root).unwrap();
    fs::create_dir(&root).unwrap();
    let replacement_root = ProviderSourceRoot::open(&root).unwrap();
    assert_ne!(
        first_root.authority_fingerprint(),
        replacement_root.authority_fingerprint()
    );
    assert!(first_root.revalidate().is_err());
}
