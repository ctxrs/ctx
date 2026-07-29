use crate::tests::support::paths::tempdir;
use crate::ProviderImportSummary;
use ctx_history_store::Store;
use std::fs;
use std::path::{Path, PathBuf};

pub(in crate::tests) fn assert_sqlite_source_file_unchanged(
    source_file: &Path,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) -> ProviderImportSummary {
    assert!(
        source_file.is_file(),
        "missing SQLite source file: {}",
        source_file.display()
    );
    let before = sqlite_file_snapshot(source_file);
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = run_import(&mut store);
    let after = sqlite_file_snapshot(source_file);
    assert_eq!(before.len(), after.len());
    for ((path, before_bytes), (after_path, after_bytes)) in before.iter().zip(after.iter()) {
        assert_eq!(path, after_path);
        assert_eq!(
            before_bytes.as_ref().map(Vec::len),
            after_bytes.as_ref().map(Vec::len),
            "SQLite source sidecar size changed for {}",
            path.display()
        );
        assert!(
            before_bytes == after_bytes,
            "SQLite source file or sidecar was mutated: {}",
            path.display()
        );
    }
    summary
}

pub(in crate::tests) fn assert_provider_source_unchanged(
    source: &Path,
    run_import: impl FnOnce(&mut Store) -> ProviderImportSummary,
) -> ProviderImportSummary {
    assert!(
        source.exists(),
        "missing provider source: {}",
        source.display()
    );
    let before = provider_source_snapshot(source);
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = run_import(&mut store);
    let after = provider_source_snapshot(source);
    assert_eq!(
        before,
        after,
        "provider source was mutated: {}",
        source.display()
    );
    summary
}

pub(in crate::tests) fn provider_source_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_dir() {
                visit(root, &path, out);
            } else if metadata.file_type().is_file() {
                out.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    fs::read(&path).unwrap(),
                ));
            } else if metadata.file_type().is_symlink() {
                out.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    fs::read_link(&path)
                        .unwrap()
                        .display()
                        .to_string()
                        .into_bytes(),
                ));
            }
        }
    }

    if root.is_file() {
        return vec![(".".to_owned(), fs::read(root).unwrap())];
    }

    let mut out = Vec::new();
    visit(root, root, &mut out);
    out
}

pub(in crate::tests) fn sqlite_file_snapshot(
    source_file: &Path,
) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    sqlite_file_snapshot_paths(source_file)
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
}

fn sqlite_file_snapshot_paths(source_file: &Path) -> Vec<PathBuf> {
    let mut paths = vec![source_file.to_path_buf()];
    // Stock read-only WAL readers may update SHM reader marks. DB, WAL, and
    // rollback-journal bytes are the persistent provider state protected here.
    for suffix in ["-wal", "-journal"] {
        let mut sidecar = source_file.as_os_str().to_os_string();
        sidecar.push(suffix);
        paths.push(PathBuf::from(sidecar));
    }
    paths
}
