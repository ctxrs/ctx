use std::{fs, path::Path};

use ctx_history_core::database_path;
use ctx_history_index::{GenerationWriter, WriterOptions};
use tempfile::tempdir;

use super::{
    complete_source_rebuild, inspect, lexical_projection_path, prepare,
    record_source_rebuild_failure, MigrationDecision, MigrationOrigin, MigrationPhase,
};

fn create_previous_store(data_root: &Path, bytes: &[u8]) -> Vec<u8> {
    fs::create_dir_all(data_root).unwrap();
    let path = database_path(data_root.to_path_buf());
    fs::write(&path, bytes).unwrap();
    fs::read(path).unwrap()
}

fn publish_empty_generation(data_root: &Path) -> String {
    GenerationWriter::open(
        lexical_projection_path(data_root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap()
    .commit(|_| true)
    .unwrap()
    .generation_id
}

#[test]
fn inspection_is_read_only_for_a_fresh_root() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("not-created");

    let marker = inspect(&data_root).unwrap().unwrap();

    assert_eq!(marker.origin, MigrationOrigin::Fresh);
    assert_eq!(marker.phase, MigrationPhase::Detected);
    assert!(!data_root.exists());
}

#[test]
fn previous_store_is_only_detected_and_never_opened_or_modified() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let before = create_previous_store(&data_root, b"not even a valid SQLite database");

    let decision = prepare(&data_root, &[]).unwrap();
    let marker = decision.marker();

    assert!(matches!(decision, MigrationDecision::RebuildFromSources(_)));
    assert_eq!(marker.origin, MigrationOrigin::PreviousHistoryStore);
    assert_eq!(marker.phase, MigrationPhase::RebuildPending);
    assert_eq!(
        marker.legacy_store_path.as_deref(),
        Some(database_path(data_root.clone()).as_path())
    );
    assert_eq!(fs::read(database_path(data_root.clone())).unwrap(), before);
    assert!(!lexical_projection_path(&data_root).exists());
}

#[test]
fn prototype_search_directory_is_not_migrated_or_reused() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let prototype_root = data_root.join("source-backed-lexical-v0");
    fs::create_dir_all(&prototype_root).unwrap();
    fs::write(prototype_root.join("prototype-marker"), b"leave untouched").unwrap();

    let prepared = prepare(&data_root, &[]).unwrap();

    assert!(prepared.daemon_rebuild_required());
    assert_eq!(
        lexical_projection_path(&data_root),
        data_root.join("search").join("lexical")
    );
    assert!(!lexical_projection_path(&data_root).exists());
    assert_eq!(
        fs::read(prototype_root.join("prototype-marker")).unwrap(),
        b"leave untouched"
    );
}

#[test]
fn activation_accepts_only_the_exact_fresh_generation() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let before = create_previous_store(&data_root, b"previous-history-remains");
    let prepared = prepare(&data_root, &[]).unwrap();
    assert!(prepared.daemon_rebuild_required());
    let generation = publish_empty_generation(&data_root);

    let mismatch = complete_source_rebuild(&data_root, &"0".repeat(64)).unwrap_err();
    assert!(format!("{mismatch:#}").contains("generation mismatch"));

    let completed = complete_source_rebuild(&data_root, &generation).unwrap();
    assert!(matches!(completed, MigrationDecision::Ready(_)));
    assert_eq!(completed.marker().phase, MigrationPhase::Ready);
    assert!(!completed.daemon_rebuild_required());
    assert_eq!(fs::read(database_path(data_root.clone())).unwrap(), before);

    let repeated = complete_source_rebuild(&data_root, &generation).unwrap();
    assert!(matches!(repeated, MigrationDecision::Ready(_)));
}

#[test]
fn failed_rebuild_is_resumable_without_touching_previous_store() {
    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    let before = create_previous_store(&data_root, b"rollback-copy");
    prepare(&data_root, &[]).unwrap();

    let failed = record_source_rebuild_failure(&data_root, "provider root unavailable").unwrap();
    assert_eq!(failed.phase, MigrationPhase::SourceRebuildFailed);
    assert!(failed.source_rebuild_required);
    assert!(failed.resumable);

    let resumed = prepare(&data_root, &[]).unwrap();
    assert_eq!(resumed.marker().phase, MigrationPhase::RebuildPending);
    assert!(resumed.marker().error.is_none());
    assert_eq!(fs::read(database_path(data_root.clone())).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn symlinked_previous_store_is_rejected_without_following_it() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    let target = temp.path().join("target");
    fs::write(&target, b"outside").unwrap();
    symlink(&target, database_path(data_root.clone())).unwrap();

    let error = inspect(&data_root).unwrap_err();
    assert!(format!("{error:#}").contains("not a regular file"));
    assert_eq!(fs::read(target).unwrap(), b"outside");
}
