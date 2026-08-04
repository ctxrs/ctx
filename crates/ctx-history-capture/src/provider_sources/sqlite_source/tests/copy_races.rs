use super::*;

#[test]
fn production_logical_acquisition_retries_commit_inside_family_copy_window() {
    assert_production_family_copy_race_retries(false);
}

#[test]
fn production_logical_acquisition_retries_checkpoint_inside_family_copy_window() {
    assert_production_family_copy_race_retries(true);
}

fn assert_production_family_copy_race_retries(checkpoint: bool) {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_large_persistent_wal(&database);
    let shared_memory = database.with_file_name("provider.sqlite-shm");
    assert!(shared_memory.exists());
    let parent = retain_parent(temp.path());
    let mut settled_provider_state = None;
    let mut attempt_starts = 0_u32;

    let snapshot = parent
        .open_logical_online_backup_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            |progress| {
                if progress.stage == SourceBackedCurrentSourceProgressStage::SourceFamilyCopy {
                    if progress.snapshot_bytes_completed == Some(0) {
                        attempt_starts += 1;
                    }
                    let inside_copy = progress
                        .snapshot_bytes_completed
                        .zip(progress.snapshot_bytes_total)
                        .is_some_and(|(completed, total)| completed > 0 && completed < total);
                    if inside_copy && settled_provider_state.is_none() {
                        if checkpoint {
                            let (busy, _, _): (i64, i64, i64) = writer
                                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                                })
                                .unwrap();
                            assert_eq!(busy, 0, "the checkpoint fixture must complete");
                            assert_eq!(
                                fs::metadata(database.with_file_name("provider.sqlite-wal"))
                                    .unwrap()
                                    .len(),
                                0,
                                "the checkpoint fixture must truncate the WAL"
                            );
                        } else {
                            writer
                                .execute("INSERT INTO messages (body) VALUES ('raced-commit')", [])
                                .unwrap();
                        }
                        settled_provider_state = Some(directory_file_bytes(temp.path()));
                    }
                }
                Ok::<(), std::convert::Infallible>(())
            },
        )
        .unwrap();

    assert_eq!(
        parent
            .snapshot_counters()
            .logical_source_transition_retries(),
        1,
        "the production path must retry exactly once"
    );
    assert_eq!(
        attempt_starts,
        if checkpoint { 1 } else { 2 },
        "the second checkpoint attempt uses the immutable main path after WAL truncation"
    );
    let settled_provider_state = settled_provider_state.unwrap();
    for expected in [
        "provider.sqlite",
        "provider.sqlite-wal",
        "provider.sqlite-shm",
    ] {
        assert!(settled_provider_state.contains_key(OsStr::new(expected)));
    }
    let values = read_values(&snapshot);
    if checkpoint {
        assert_eq!(values, ["before-wal", "from-wal"]);
    } else {
        assert_eq!(values, ["before-wal", "from-wal", "raced-commit"]);
    }
    snapshot.finish().unwrap();
    assert_eq!(directory_file_bytes(temp.path()), settled_provider_state);
}
