use crate::test_support_paths::capture_manifest_dir;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub(in crate::tests) fn tempdir() -> TempDir {
    crate::test_support_paths::tempdir()
        .expect("system temporary directory should support test fixtures")
}

#[test]
fn test_tempdir_has_no_symlinked_parent_components() {
    let temp = tempdir();
    crate::common::io::ensure_provider_path_parents_are_not_symlinks(
        &temp.path().join("provider-transcript.jsonl"),
    )
    .unwrap();
}

pub(in crate::tests) fn provider_fixture(name: &str) -> PathBuf {
    materialized_fixture("provider", name)
}

pub(in crate::tests) fn provider_history_fixture(name: &str) -> PathBuf {
    materialized_fixture("provider-history", name)
}

pub(in crate::tests) fn custom_history_fixture(name: &str) -> PathBuf {
    materialized_fixture("custom-history-jsonl", name)
}

pub(in crate::tests) fn materialized_fixture(category: &str, name: &str) -> PathBuf {
    let manifest_dir = capture_manifest_dir();
    let source = match category {
        "provider" => manifest_dir
            .join("../../tests/fixtures/provider")
            .join(name),
        "provider-history" => manifest_dir
            .join("../../tests/fixtures/provider-history")
            .join(name),
        "custom-history-jsonl" => manifest_dir
            .join("../../tests/fixtures/custom-history-jsonl")
            .join(name),
        _ => panic!("unknown fixture category {category}"),
    };
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| PathBuf::from(path).join("test-data/materialized-fixtures"))
        .unwrap_or_else(|| manifest_dir.join("../../target/test-data/materialized-fixtures"));
    fs::create_dir_all(&root).unwrap();
    let unique = format!(
        "{}-{}-{}-{}",
        category,
        name.replace(['/', '\\', '.'], "_"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = root.join(unique);
    if source.is_dir() {
        copy_dir_all(&source, &target);
    } else {
        fs::copy(&source, &target).unwrap();
    }
    target
}

pub(in crate::tests) fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}
