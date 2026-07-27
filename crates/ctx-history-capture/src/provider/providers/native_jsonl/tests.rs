use std::{ffi::OsStr, fs, path::Path};

use ctx_history_core::CaptureProvider;

use crate::test_support_paths::tempdir;

use super::{traversal, visit_native_jsonl_files};

#[test]
fn tree_visitation_is_sorted_by_durable_filename_bytes() {
    fn visited_agents(root: &Path, creation_order: &[&str]) -> Vec<String> {
        for agent in creation_order {
            let directory = root.join("sessions/work/session/agents").join(agent);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("wire.jsonl"), b"\n").unwrap();
        }

        let mut visited = Vec::new();
        visit_native_jsonl_files(root, CaptureProvider::KimiCodeCli, &mut |path| {
            visited.push(
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(OsStr::to_str)
                    .unwrap()
                    .to_owned(),
            );
            Ok(())
        })
        .unwrap();
        visited
    }

    let first = tempdir().unwrap();
    let second = tempdir().unwrap();
    let expected = vec!["agent-1".to_owned(), "main".to_owned()];
    assert_eq!(visited_agents(first.path(), &["main", "agent-1"]), expected);
    assert_eq!(
        visited_agents(second.path(), &["agent-1", "main"]),
        expected
    );
}

#[test]
fn wide_tree_visitation_is_single_scan_bounded_and_globally_sorted() {
    const ENTRY_COUNT: usize = 1_025;

    let temp = tempdir().unwrap();
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
    let (result, stats) = traversal::count_native_jsonl_traversal_work(|| {
        visit_native_jsonl_files(&root, CaptureProvider::Codex, &mut |path| {
            visited.push(path.file_name().unwrap().to_str().unwrap().to_owned());
            Ok(())
        })
    });
    assert_eq!(result.unwrap(), ENTRY_COUNT);
    assert_eq!(visited, expected);
    assert_eq!(stats.directory_read_passes, 1);
    assert_eq!(stats.directory_entries_read, ENTRY_COUNT);
    assert_eq!(stats.max_retained_names, 64);
    assert_eq!(stats.initial_runs, 17);
    assert_eq!(stats.max_merge_readers, 16);
    assert_eq!(stats.merge_names_read, ENTRY_COUNT * 2);
    assert_eq!(stats.final_names_read, ENTRY_COUNT);
}
