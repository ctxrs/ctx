use std::ffi::OsStr;

#[cfg(unix)]
use std::os::unix::fs::symlink;

use super::*;

#[test]
fn wide_tree_visitation_is_single_scan_bounded_and_globally_sorted() {
    const ENTRY_COUNT: usize = 1_025;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(&root).unwrap();
    let mut expected = (0..ENTRY_COUNT)
        .map(|index| format!("session-{index:04}.jsonl"))
        .collect::<Vec<_>>();
    for name in expected.iter().rev() {
        fs::write(root.join(name), b"\n").unwrap();
    }
    expected.sort();

    let mut visited = Vec::new();
    let (result, stats) = count_bounded_tree_traversal_work(|| {
        visit_bounded_tree_files(
            &root,
            &mut |candidate| candidate.path().extension() == Some(OsStr::new("jsonl")),
            &mut |source_file| {
                visited.push(
                    source_file
                        .path()
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned(),
                );
                Ok::<(), SourceIoError>(())
            },
        )
    });

    assert_eq!(result.unwrap(), ENTRY_COUNT);
    assert_eq!(visited, expected);
    assert_eq!(stats.directory_read_passes, 1);
    assert_eq!(stats.directory_entries_read, ENTRY_COUNT);
    assert_eq!(stats.max_retained_names, 64);
    assert_eq!(stats.initial_runs, if cfg!(windows) { 34 } else { 17 });
    assert_eq!(stats.max_merge_readers, 16);
    assert_eq!(
        stats.merge_names_read,
        ENTRY_COUNT * if cfg!(windows) { 4 } else { 2 }
    );
    assert_eq!(stats.final_names_read, ENTRY_COUNT);
}

#[test]
fn windows_aliases_across_runs_are_deduplicated_before_selection() {
    let mut visited = Vec::new();
    let mut selected = Vec::new();
    let (result, stats) = count_bounded_tree_traversal_work(|| {
        let mut names = BoundedTreeNames::new(BoundedTreeRunOrder::WindowsAsciiCaseEquivalent);
        names
            .push_key(windows_test_filename_key("a.jsonl"))
            .unwrap();
        for index in 0..63 {
            names
                .push_key(windows_test_filename_key(&format!("m-{index:02}.txt")))
                .unwrap();
        }
        // The alias starts the next 64-name run, so only the bounded merge
        // can retain the first spelling before child selection.
        names
            .push_key(windows_test_filename_key("A.jsonl"))
            .unwrap();
        names
            .push_key(windows_test_filename_key("B.jsonl"))
            .unwrap();
        names.visit_keys(&mut |key| {
            let name = windows_test_filename_from_key(&key);
            if name.ends_with(".jsonl") {
                selected.push(name.clone());
            }
            visited.push(name);
            Ok::<(), SourceIoError>(())
        })
    });

    result.unwrap();
    let mut expected = (0..63)
        .map(|index| format!("m-{index:02}.txt"))
        .chain(["a.jsonl".to_owned(), "B.jsonl".to_owned()])
        .collect::<Vec<_>>();
    expected.sort_by_key(|name| windows_test_filename_key(name));
    assert_eq!(visited, expected);
    assert_eq!(selected, ["B.jsonl", "a.jsonl"]);
    assert_eq!(stats.max_retained_names, 64);
    assert_eq!(stats.initial_runs, 4);
    assert_eq!(stats.max_merge_readers, 2);
    assert_eq!(stats.merge_names_read, 131);
    assert_eq!(stats.final_names_read, 65);
}

fn windows_test_filename_key(name: &str) -> Vec<u8> {
    name.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn windows_test_filename_from_key(key: &[u8]) -> String {
    String::from_utf16(&windows_filename_units(key).collect::<Vec<_>>()).unwrap()
}

#[test]
#[cfg(unix)]
fn selected_child_failure_is_isolated_from_healthy_siblings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a-healthy.jsonl"), b"{}\n").unwrap();
    symlink("a-healthy.jsonl", root.join("b-rejected.jsonl")).unwrap();

    let mut visited = Vec::new();
    let mut failures = Vec::new();
    let count = visit_bounded_tree_files_isolating_selected(
        &root,
        &mut |candidate| candidate.path().extension() == Some(OsStr::new("jsonl")),
        &mut |source_file| {
            visited.push(source_file.path().file_name().unwrap().to_owned());
            Ok::<(), SourceIoError>(())
        },
        &mut |path, _error| {
            failures.push(path.file_name().unwrap().to_owned());
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count, 1);
    assert_eq!(visited, [OsString::from("a-healthy.jsonl")]);
    assert_eq!(failures, [OsString::from("b-rejected.jsonl")]);
}

#[test]
#[cfg(unix)]
fn rejected_child_directory_is_isolated_from_healthy_siblings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    let reachable = root.join("a-reachable");
    fs::create_dir_all(&reachable).unwrap();
    fs::write(reachable.join("kept.jsonl"), b"{}\n").unwrap();
    // A symlinked directory carries no `.jsonl` extension, so selection
    // never claims it and the traversal used to fail the entire scan here.
    symlink(&reachable, root.join("b-linked-dir")).unwrap();
    fs::write(root.join("c-kept.jsonl"), b"{}\n").unwrap();

    let mut visited = Vec::new();
    let mut isolated = 0;
    let count = visit_bounded_tree_files_isolating_selected(
        &root,
        &mut |candidate| candidate.path().extension() == Some(OsStr::new("jsonl")),
        &mut |source_file| {
            visited.push(source_file.path().file_name().unwrap().to_owned());
            Ok::<(), SourceIoError>(())
        },
        &mut |_path, _error| {
            isolated += 1;
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(count, 2);
    visited.sort();
    assert_eq!(
        visited,
        [OsString::from("c-kept.jsonl"), OsString::from("kept.jsonl")]
    );
    assert_eq!(isolated, 0);
}

#[test]
fn ordinary_unselected_child_open_failure_is_fatal() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    let raced = root.join("raced-away");
    fs::create_dir_all(&raced).unwrap();
    let mut isolated = 0;

    let error = visit_bounded_tree_files_isolating_selected(
        &root,
        &mut |candidate| {
            if candidate.path() == raced {
                fs::remove_dir(&raced).unwrap();
            }
            false
        },
        &mut |_| Ok::<(), SourceIoError>(()),
        &mut |_path, _error| {
            isolated += 1;
            Ok(())
        },
    )
    .unwrap_err();

    assert!(matches!(error, SourceIoError::Io(error) if error.kind() == io::ErrorKind::NotFound));
    assert_eq!(isolated, 0);
}

#[test]
#[cfg(unix)]
fn root_failure_is_never_downgraded_to_a_selected_file_failure() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("target");
    fs::write(&target, b"{}\n").unwrap();
    let root = directory.path().join("root.jsonl");
    symlink(&target, &root).unwrap();
    let mut isolated = 0;

    let error = visit_bounded_tree_files_isolating_selected(
        &root,
        &mut |_| true,
        &mut |_| Ok::<(), SourceIoError>(()),
        &mut |_path, _error| {
            isolated += 1;
            Ok(())
        },
    )
    .unwrap_err();

    assert!(matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath { .. }
    ));
    assert_eq!(isolated, 0);
}

#[test]
fn directory_depth_limit_keeps_the_existing_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("tree");
    let mut deepest = root.clone();
    for _ in 0..=BOUNDED_TREE_MAX_DIRECTORY_DEPTH {
        deepest.push("d");
    }
    fs::create_dir_all(&deepest).unwrap();

    let error =
        visit_bounded_tree_files(&root, &mut |_| false, &mut |_| Ok::<(), SourceIoError>(()))
            .unwrap_err();

    assert!(matches!(
        error,
        SourceIoError::InvalidProviderTranscriptPath {
            reason: "provider transcript directory nesting exceeds the supported limit",
            ..
        }
    ));
}

#[test]
fn frozen_tree_defers_later_names_and_allows_growth_but_exact_tree_rejects_growth() {
    for frozen in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let leaf = temp.path().join("first.jsonl");
        fs::write(&leaf, b"first\n").unwrap();
        let mut visited = Vec::new();
        let mut visitor = |source: BoundedTreeSourceFile| {
            visited.push(source.path().to_path_buf());
            fs::OpenOptions::new()
                .append(true)
                .open(&leaf)
                .unwrap()
                .write_all(b"second\n")
                .unwrap();
            if frozen {
                fs::write(temp.path().join("later.jsonl"), b"later\n").unwrap();
            }
            Ok::<_, SourceIoError>(())
        };
        let result = if frozen {
            visit_bounded_tree_files_frozen(
                temp.path(),
                &mut |_| true,
                &mut visitor,
                &mut |_, error| Err(error),
            )
        } else {
            visit_bounded_tree_files(temp.path(), &mut |_| true, &mut visitor)
        };
        if frozen {
            assert_eq!(result.unwrap(), 1);
            assert_eq!(visited, [leaf]);
            assert_eq!(
                visit_bounded_tree_files_frozen(
                    temp.path(),
                    &mut |_| true,
                    &mut |_| Ok::<_, SourceIoError>(()),
                    &mut |_, error| Err(error)
                )
                .unwrap(),
                2
            );
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn directory_snapshot_rejects_mutation_during_enumeration_and_named_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let child = root.join("child");
    fs::create_dir_all(&child).unwrap();
    fs::write(child.join("first"), b"first").unwrap();
    // Creation and the callback can land in the same filesystem clock tick.
    // Give the directory an older mtime so the mutation is observable without
    // a timing-dependent sleep (Unix permits opening a directory as a File).
    #[cfg(unix)]
    fs::File::open(&child)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
        .unwrap();
    let authority = crate::ProviderSourceRoot::open(&root).unwrap();
    let directory = authority.open_directory(Path::new("child")).unwrap();
    let mut changed = false;
    let result = directory.visit_entries_snapshot(10, |_| {
        if !changed {
            fs::write(child.join("second"), b"second").unwrap();
            changed = true;
        }
        Ok::<_, SourceIoError>(())
    });
    assert!(result.is_err());
    assert_eq!(directory.entries_snapshot(10).unwrap().len(), 2);
    fs::rename(&child, root.join("old-child")).unwrap();
    fs::create_dir(&child).unwrap();
    assert!(directory.revalidate_same_object().is_err());
}
