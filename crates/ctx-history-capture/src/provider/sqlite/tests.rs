mod tests {
    use std::{
        cell::Cell,
        fs,
        io::{self, Write},
        path::Path,
        rc::Rc,
    };

    use rusqlite::{params, types::Value as SqlValue, Connection};

    use crate::provider::sqlite_observation::{
        install_sqlite_observation_test_hook, SqliteObservationTestPhase,
        SQLITE_GENERATION_MAX_ATTEMPTS,
    };
    #[cfg(unix)]
    use crate::CaptureError;
    use crate::{install_disk_io_pacer, DiskIoPacer};

    use super::{
        acquire_snapshot_copy_lock, create_private_snapshot_dir_in, create_private_snapshot_file,
        install_sqlite_pinned_open_test_hook, install_sqlite_probe_test_hook,
        install_sqlite_snapshot_copy_test_hook, install_sqlite_snapshot_test_hook,
        observe_sqlite_source_generation, open_sqlite_readonly_source, optional_text_column_expr,
        optional_timestamp_millis_expr, probe_sqlite_readonly_source,
        take_sqlite_snapshot_test_metrics, validate_snapshot_available_space,
        validate_snapshot_ceiling, BTreeSet, SqlitePinnedOpenTestPhase, SqliteSnapshotTestMetrics,
        SQLITE_SNAPSHOT_DISK_RESERVE_BYTES, SQLITE_SNAPSHOT_MAX_BYTES,
    };
    #[cfg(unix)]
    use super::{pinned_identity_open_unavailable, recover_unavailable_pinned_open};

    #[cfg(windows)]
    use super::sqlite_readonly_uri;

    #[test]
    fn optional_sqlite_casts_normalize_native_text_and_timestamp_shapes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE samples (position INTEGER, value)", [])
            .unwrap();
        let samples = [
            (SqlValue::Integer(1_783_653_514), Some(1_783_653_514_000)),
            (SqlValue::Real(1_783_653_514.491), Some(1_783_653_514_491)),
            (
                SqlValue::Integer(1_783_653_514_491),
                Some(1_783_653_514_491),
            ),
            (SqlValue::Real(1_783_653_514_491.0), Some(1_783_653_514_491)),
            (SqlValue::Text("1783653514".into()), Some(1_783_653_514_000)),
            (
                SqlValue::Text("+1783653514".into()),
                Some(1_783_653_514_000),
            ),
            (SqlValue::Text("-1.25".into()), Some(-1_250)),
            (
                SqlValue::Text("1783653514.491".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("1783653514491".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("0001783653514".into()),
                Some(1_783_653_514_000),
            ),
            (
                SqlValue::Text("2026-07-10T03:18:34.491Z".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("2026-07-10T05:48:34.491+02:30".into()),
                Some(1_783_653_514_491),
            ),
            (SqlValue::Text("not-a-timestamp".into()), None),
            (SqlValue::Text("  ".into()), None),
            (SqlValue::Null, None),
        ];
        for (position, (value, _)) in samples.iter().enumerate() {
            conn.execute(
                "INSERT INTO samples VALUES (?1, ?2)",
                params![position as i64, value],
            )
            .unwrap();
        }

        let columns = BTreeSet::from(["value".to_owned()]);
        let timestamp = optional_timestamp_millis_expr(&columns, "value", "NULL");
        let sql = format!("SELECT {timestamp} FROM samples ORDER BY position");
        let actual = conn
            .prepare(&sql)
            .unwrap()
            .query_map([], |row| row.get::<_, Option<i64>>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            actual,
            samples
                .iter()
                .map(|(_, expected)| *expected)
                .collect::<Vec<_>>()
        );

        let text = optional_text_column_expr(&columns, "value", "NULL");
        let value: String = conn
            .query_row(
                &format!("SELECT {text} FROM samples WHERE position = 0"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "1783653514");

        let missing = BTreeSet::new();
        assert_eq!(
            optional_timestamp_millis_expr(&missing, "value", "fallback"),
            "fallback"
        );
        assert_eq!(
            optional_text_column_expr(&missing, "value", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn checkpointed_wal_retries_as_a_new_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let writer = real_wal_writer(&db);
        let _hook = install_sqlite_snapshot_test_hook(move |_, attempt| {
            if attempt == 1 {
                writer
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                    .unwrap();
            }
        });

        take_sqlite_snapshot_test_metrics();
        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics {
                attempts: 2,
                copied_files: 3,
            }
        );
    }

    #[test]
    fn snapshot_copy_accounts_source_and_destination_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("paced-snapshot.db");
        let _writer = real_wal_writer(&db);
        let generation = observe_sqlite_source_generation(&db).unwrap();
        let snapshot_bytes = generation
            .snapshot_files()
            .into_iter()
            .map(|file| file.snapshot_len())
            .sum::<u64>();
        let pacer = DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = install_disk_io_pacer(pacer.clone());

        let connection = open_sqlite_readonly_source(&db).unwrap();
        drop(connection);

        assert!(pacer.charged_bytes() >= snapshot_bytes.saturating_mul(2));
    }

    #[test]
    fn public_open_recovers_the_last_committed_prefix_before_a_bad_wal_frame() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("valid-prefix.db");
        let writer = real_wal_writer(&db);
        let wal = sidecar(&db, "-wal");
        let committed_prefix_len = fs::metadata(&wal).unwrap().len();
        writer
            .execute("UPDATE entries SET value = 'sigma' WHERE id = 1", [])
            .unwrap();
        let mut wal_bytes = fs::read(&wal).unwrap();
        assert!(wal_bytes.len() as u64 > committed_prefix_len);
        wal_bytes[committed_prefix_len as usize + 24] ^= 0x01;
        fs::write(&wal, wal_bytes).unwrap();

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "omega"
        );
    }

    #[test]
    fn stable_corrupt_wal_is_terminal_on_public_open_and_probe_paths() {
        for probe in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("corrupt.db");
            let _writer = real_wal_writer(&db);
            let wal = sidecar(&db, "-wal");
            let mut wal_bytes = fs::read(&wal).unwrap();
            wal_bytes[24] ^= 0x01;
            fs::write(&wal, wal_bytes).unwrap();

            let result: crate::Result<()> = if probe {
                probe_sqlite_readonly_source(&db, |_| Ok(true)).map(|_| ())
            } else {
                open_sqlite_readonly_source(&db).map(drop)
            };
            let error = result.unwrap_err();
            assert!(matches!(
                error,
                crate::CaptureError::Io(ref error)
                    if error.kind() == io::ErrorKind::InvalidData
                        && error.to_string().contains("header checksum")
            ));
        }
    }

    #[test]
    fn stable_unsupported_wal_version_is_terminal_on_public_open_and_probe_paths() {
        for probe in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("unsupported-version.db");
            let _writer = real_wal_writer(&db);
            let wal = sidecar(&db, "-wal");
            let mut wal_bytes = fs::read(&wal).unwrap();
            rewrite_wal_format_version(&mut wal_bytes, 3_007_001);
            fs::write(&wal, wal_bytes).unwrap();

            let result: crate::Result<()> = if probe {
                probe_sqlite_readonly_source(&db, |_| Ok(true)).map(|_| ())
            } else {
                open_sqlite_readonly_source(&db).map(drop)
            };
            let error = result.unwrap_err();
            assert!(matches!(
                error,
                crate::CaptureError::Io(ref error)
                    if error.kind() == io::ErrorKind::InvalidData
                        && error.to_string().contains("format version")
            ));
        }
    }

    #[test]
    fn public_open_and_probe_retry_a_transiently_missing_required_main() {
        for probe in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("appearing.db");
            let opens = Rc::new(Cell::new(0_usize));
            let opens_for_hook = Rc::clone(&opens);
            let db_for_hook = db.clone();
            let _hook = install_sqlite_observation_test_hook(move |path, phase| {
                if path != db_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                    return;
                }
                let count = opens_for_hook.get() + 1;
                opens_for_hook.set(count);
                if count == 2 {
                    write_single_value_db(path, "appeared");
                }
            });

            if probe {
                assert!(probe_sqlite_readonly_source(&db, |conn| {
                    conn.query_row("SELECT value = 'appeared' FROM entries", [], |row| {
                        row.get::<_, bool>(0)
                    })
                })
                .unwrap());
            } else {
                let connection = open_sqlite_readonly_source(&db).unwrap();
                assert_eq!(
                    connection
                        .query_row("SELECT value FROM entries", [], |row| row
                            .get::<_, String>(0))
                        .unwrap(),
                    "appeared"
                );
            }
            assert!(opens.get() >= 3);
        }
    }

    #[test]
    fn public_open_and_probe_preserve_stable_required_main_not_found() {
        for probe in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("missing.db");
            let opens = Rc::new(Cell::new(0_usize));
            let opens_for_hook = Rc::clone(&opens);
            let db_for_hook = db.clone();
            let _hook = install_sqlite_observation_test_hook(move |path, phase| {
                if path == db_for_hook && phase == SqliteObservationTestPhase::BeforeOpen {
                    opens_for_hook.set(opens_for_hook.get() + 1);
                }
            });

            let result: crate::Result<()> = if probe {
                probe_sqlite_readonly_source(&db, |_| Ok(true)).map(|_| ())
            } else {
                open_sqlite_readonly_source(&db).map(drop)
            };
            let error = result.unwrap_err();
            assert!(matches!(
                error,
                crate::CaptureError::Io(ref error) if error.kind() == io::ErrorKind::NotFound
            ));
            assert_eq!(opens.get(), SQLITE_GENERATION_MAX_ATTEMPTS);
        }
    }

    #[test]
    fn wal_truncation_during_snapshot_copy_retries_without_terminal_error() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("copy-truncate.db");
        let _writer = real_wal_writer(&db);
        let wal = sidecar(&db, "-wal");
        let truncated = Rc::new(Cell::new(false));
        let truncated_for_hook = Rc::clone(&truncated);
        let _hook = install_sqlite_snapshot_copy_test_hook(move |path| {
            if path == wal && !truncated_for_hook.replace(true) {
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
            }
        });

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert!(truncated.get());
        assert!(connection._snapshot_dir.is_some());
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "alpha"
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_copy_uses_observed_handle_across_symlink_swap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("copy-swap.db");
        let _writer = real_wal_writer(&db);
        let wal = sidecar(&db, "-wal");
        let held = temp.path().join("held.wal");
        let outside = temp.path().join("outside.wal");
        fs::write(
            &outside,
            vec![0xa5; fs::metadata(&wal).unwrap().len() as usize],
        )
        .unwrap();
        let swapped = Rc::new(Cell::new(false));
        let swapped_for_copy = Rc::clone(&swapped);
        let wal_for_copy = wal.clone();
        let held_for_copy = held.clone();
        let outside_for_copy = outside.clone();
        let _copy_hook = install_sqlite_snapshot_copy_test_hook(move |path| {
            if path == wal_for_copy && !swapped_for_copy.replace(true) {
                fs::rename(path, &held_for_copy).unwrap();
                symlink(&outside_for_copy, path).unwrap();
            }
        });
        let swapped_for_restore = Rc::clone(&swapped);
        let wal_for_restore = wal.clone();
        let _restore_hook = install_sqlite_snapshot_test_hook(move |_, _| {
            if swapped_for_restore.replace(false) {
                fs::remove_file(&wal_for_restore).unwrap();
                fs::rename(&held, &wal_for_restore).unwrap();
            }
        });

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert!(!swapped.get());
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "omega"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pinned_open_uses_observed_descriptor_across_restored_parent_symlink_swap() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let source_dir = temp.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        let db = source_dir.join("pinned.db");
        write_single_value_db(&db, "inside");
        let baseline = observe_sqlite_source_generation(&db).unwrap();
        let outside_dir = temp.path().join("outside");
        fs::create_dir(&outside_dir).unwrap();
        let outside = outside_dir.join("pinned.db");
        write_single_value_db(&outside, "outside");
        let held_dir = temp.path().join("held-source");
        let attempted = Rc::new(Cell::new(false));
        let swapped = Rc::new(Cell::new(false));
        let open_calls = Rc::new(Cell::new(0_usize));
        let attempted_for_hook = Rc::clone(&attempted);
        let swapped_for_hook = Rc::clone(&swapped);
        let open_calls_for_hook = Rc::clone(&open_calls);
        let db_for_hook = db.clone();
        let _hook = install_sqlite_pinned_open_test_hook(move |path, phase| {
            if path != db_for_hook {
                return;
            }
            match phase {
                SqlitePinnedOpenTestPhase::BeforeOpen => {
                    open_calls_for_hook.set(open_calls_for_hook.get() + 1);
                    if !attempted_for_hook.replace(true) {
                        fs::rename(&source_dir, &held_dir).unwrap();
                        symlink(&outside_dir, &source_dir).unwrap();
                        swapped_for_hook.set(true);
                    }
                }
                SqlitePinnedOpenTestPhase::AfterOpen if swapped_for_hook.replace(false) => {
                    fs::remove_file(&source_dir).unwrap();
                    fs::rename(&held_dir, &source_dir).unwrap();
                }
                _ => {}
            }
        });

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert!(attempted.get());
        assert_eq!(open_calls.get(), 1);
        assert!(!swapped.get());
        assert!(connection._snapshot_dir.is_none());
        assert_eq!(observe_sqlite_source_generation(&db).unwrap(), baseline);
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "inside"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_identity_descriptor_falls_back_to_observed_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        write_single_value_db(&db, "inside");
        let generation = observe_sqlite_source_generation(&db).unwrap();

        assert!(pinned_identity_open_unavailable(&CaptureError::Io(
            io::Error::new(
                io::ErrorKind::Unsupported,
                "descriptor namespace unavailable"
            )
        )));
        let descriptor_cantopen = CaptureError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("descriptor is not exposed by the mounted filesystem".into()),
        ));
        assert!(pinned_identity_open_unavailable(&descriptor_cantopen));
        take_sqlite_snapshot_test_metrics();
        let connection = recover_unavailable_pinned_open(&db, &generation, descriptor_cantopen)
            .unwrap()
            .expect("expected stable SQLite connection");
        assert!(connection._snapshot_dir.is_some());
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics {
                attempts: 1,
                copied_files: 1,
            }
        );
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "inside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn pinned_open_blocks_parent_junction_swap_until_sqlite_opens_observed_file() {
        let temp = tempfile::tempdir().unwrap();
        let source_dir = temp.path().join("source");
        fs::create_dir(&source_dir).unwrap();
        let db = source_dir.join("pinned.db");
        write_single_value_db(&db, "inside");
        let outside_dir = temp.path().join("outside");
        fs::create_dir(&outside_dir).unwrap();
        write_single_value_db(&outside_dir.join("pinned.db"), "outside");
        let held_dir = temp.path().join("held-source");
        let attempted = Rc::new(Cell::new(false));
        let blocked = Rc::new(Cell::new(false));
        let swapped = Rc::new(Cell::new(false));
        let attempted_for_hook = Rc::clone(&attempted);
        let blocked_for_hook = Rc::clone(&blocked);
        let swapped_for_hook = Rc::clone(&swapped);
        let db_for_hook = db.clone();
        let source_for_hook = source_dir.clone();
        let held_for_hook = held_dir.clone();
        let outside_for_hook = outside_dir.clone();
        let _hook = install_sqlite_pinned_open_test_hook(move |path, phase| {
            if path != db_for_hook {
                return;
            }
            match phase {
                SqlitePinnedOpenTestPhase::BeforeOpen if !attempted_for_hook.replace(true) => {
                    match fs::rename(&source_for_hook, &held_for_hook) {
                        Ok(()) => {
                            create_windows_junction(&source_for_hook, &outside_for_hook);
                            swapped_for_hook.set(true);
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::PermissionDenied
                                || matches!(error.raw_os_error(), Some(5 | 32)) =>
                        {
                            blocked_for_hook.set(true);
                        }
                        Err(error) => panic!("unexpected parent swap failure: {error}"),
                    }
                }
                SqlitePinnedOpenTestPhase::AfterOpen if swapped_for_hook.replace(false) => {
                    fs::remove_dir(&source_for_hook).unwrap();
                    fs::rename(&held_for_hook, &source_for_hook).unwrap();
                }
                _ => {}
            }
        });

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert!(attempted.get());
        assert!(blocked.get());
        assert!(!swapped.get());
        assert!(connection._snapshot_dir.is_none());
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "inside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn readonly_uri_round_trips_windows_disk_and_unc_paths() {
        for path in [
            std::path::PathBuf::from(r"C:\Users\ctx\history.db"),
            std::path::PathBuf::from(r"\\server\share\history.db"),
        ] {
            let uri = sqlite_readonly_uri(&path).unwrap();
            let mut url = url::Url::parse(&uri).unwrap();
            assert_eq!(
                url.query_pairs().collect::<Vec<_>>(),
                [("mode".into(), "ro".into())]
            );
            url.set_query(None);
            assert_eq!(url.to_file_path().unwrap(), path);
        }
    }

    #[test]
    fn real_wal_snapshot_is_private_query_only_and_raii_cleaned() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let _writer = real_wal_writer(&db);

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "omega"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let snapshot = connection
            ._snapshot_dir
            .as_ref()
            .expect("WAL requires a snapshot")
            .path()
            .to_path_buf();
        assert!(snapshot.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(snapshot.join("source.db"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(connection);
        assert!(!snapshot.exists());
    }

    #[test]
    fn rollback_main_with_shm_only_does_not_copy() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE entries (id INTEGER)", [])
            .unwrap();
        drop(conn);
        fs::write(sidecar(&db, "-shm"), b"volatile coordination state").unwrap();

        take_sqlite_snapshot_test_metrics();
        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM entries", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(connection._snapshot_dir.is_none());
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics::default()
        );
    }

    #[test]
    fn rollback_main_reader_holds_a_coherent_transaction() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        write_single_value_db(&db, "alpha");

        let reader = open_sqlite_readonly_source(&db).unwrap();
        assert!(reader._snapshot_dir.is_none());
        let writer = Connection::open(&db).unwrap();
        writer.busy_timeout(std::time::Duration::ZERO).unwrap();
        let error = writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap_err();
        assert!(matches!(
            error,
            rusqlite::Error::SqliteFailure(ref error, _)
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                )
        ));
        assert_eq!(
            reader
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "alpha"
        );
        drop(reader);
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
    }

    #[test]
    fn wal_mode_without_committed_sidecar_uses_a_coherent_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        drop(writer);

        take_sqlite_snapshot_test_metrics();
        let reader = open_sqlite_readonly_source(&db).unwrap();
        assert!(reader._snapshot_dir.is_some());
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics {
                attempts: 1,
                copied_files: 1,
            }
        );
        let writer = Connection::open(&db).unwrap();
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        writer
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        assert_eq!(
            reader
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "alpha"
        );
    }

    #[test]
    fn copied_readonly_main_is_writable_for_hot_journal_recovery() {
        let fixture = real_hot_journal_fixture();
        let mut permissions = fs::metadata(&fixture.db).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&fixture.db, permissions).unwrap();

        let connection = open_sqlite_readonly_source(&fixture.db).unwrap();
        let restored: i64 = connection
            .query_row(
                "SELECT count(*) FROM entries WHERE substr(value, 1, 1) = 'a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(restored, 256);
        drop(connection);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&fixture.db, fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(&fixture.db).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&fixture.db, permissions).unwrap();
        }
    }

    #[test]
    fn hot_journal_with_attached_database_super_pointer_defers() {
        let fixture = real_hot_journal_fixture();
        let journal = sidecar(&fixture.db, "-journal");
        let super_journal = fixture
            .db
            .parent()
            .unwrap()
            .join("attached-main.db-mj H8a1");
        fs::write(&super_journal, b"active multi-database commit").unwrap();
        append_super_journal_trailer(&journal, &native_path_bytes(&super_journal));

        let error = match open_sqlite_readonly_source(&fixture.db) {
            Ok(_) => panic!("super-journal generation was imported"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            crate::CaptureError::Io(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn sqlite_probe_retries_both_negative_and_positive_races() {
        for starts_present in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("probe.db");
            let conn = Connection::open(&db).unwrap();
            if starts_present {
                conn.execute("CREATE TABLE target (id INTEGER)", [])
                    .unwrap();
            } else {
                conn.execute("CREATE TABLE baseline (id INTEGER)", [])
                    .unwrap();
            }
            drop(conn);

            let changed = Rc::new(Cell::new(false));
            let changed_for_hook = Rc::clone(&changed);
            let _hook = install_sqlite_probe_test_hook(move |path| {
                if changed_for_hook.replace(true) {
                    return;
                }
                let conn = Connection::open(path).unwrap();
                if starts_present {
                    conn.execute("DROP TABLE target", []).unwrap();
                } else {
                    conn.execute("CREATE TABLE target (id INTEGER)", [])
                        .unwrap();
                }
            });
            let found = probe_sqlite_readonly_source(&db, |conn| {
                conn.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'target'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count == 1)
            })
            .unwrap();
            assert_eq!(found, !starts_present);
        }
    }

    #[test]
    fn snapshot_resource_limits_are_retryable() {
        let ceiling = validate_snapshot_ceiling(SQLITE_SNAPSHOT_MAX_BYTES + 1).unwrap_err();
        assert!(matches!(
            ceiling,
            crate::CaptureError::Io(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        let disk =
            validate_snapshot_available_space(1, SQLITE_SNAPSHOT_DISK_RESERVE_BYTES).unwrap_err();
        assert!(matches!(
            disk,
            crate::CaptureError::Io(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn snapshot_copy_lock_serializes_space_checks_and_copies() {
        use std::{sync::mpsc, thread, time::Duration};

        let parent = tempfile::tempdir().unwrap();
        let first = acquire_snapshot_copy_lock(parent.path()).unwrap();
        let path = parent.path().to_path_buf();
        let (sender, receiver) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let second = acquire_snapshot_copy_lock(&path).unwrap();
            sender.send(()).unwrap();
            drop(second);
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(first);
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_snapshot_creation_is_owner_only_with_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let old_umask = unsafe { libc::umask(0) };
        let created = (|| -> std::io::Result<_> {
            let dir = create_private_snapshot_dir_in(parent.path())?;
            let file = dir.path().join("source.db");
            drop(create_private_snapshot_file(&file)?);
            Ok((dir, file))
        })();
        unsafe {
            libc::umask(old_umask);
        }
        let (dir, file) = created.unwrap();

        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    fn real_wal_writer(path: &Path) -> Connection {
        let writer = Connection::open(path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        assert!(sidecar(path, "-wal").is_file());
        writer
    }

    fn write_single_value_db(path: &Path, value: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);")
            .unwrap();
        connection
            .execute("INSERT INTO entries VALUES (1, ?1)", [value])
            .unwrap();
    }

    fn rewrite_wal_format_version(bytes: &mut [u8], version: u32) {
        bytes[4..8].copy_from_slice(&version.to_be_bytes());
        let little_endian = match u32::from_be_bytes(bytes[0..4].try_into().unwrap()) {
            0x377f_0682 => true,
            0x377f_0683 => false,
            magic => panic!("unexpected SQLite WAL magic {magic:#x}"),
        };
        let mut s1 = 0_u32;
        let mut s2 = 0_u32;
        for words in bytes[..24].chunks_exact(8) {
            let first = if little_endian {
                u32::from_le_bytes(words[0..4].try_into().unwrap())
            } else {
                u32::from_be_bytes(words[0..4].try_into().unwrap())
            };
            let second = if little_endian {
                u32::from_le_bytes(words[4..8].try_into().unwrap())
            } else {
                u32::from_be_bytes(words[4..8].try_into().unwrap())
            };
            s1 = s1.wrapping_add(first).wrapping_add(s2);
            s2 = s2.wrapping_add(second).wrapping_add(s1);
        }
        bytes[24..28].copy_from_slice(&s1.to_be_bytes());
        bytes[28..32].copy_from_slice(&s2.to_be_bytes());
    }

    struct HotJournalFixture {
        _temp: tempfile::TempDir,
        db: std::path::PathBuf,
    }

    fn real_hot_journal_fixture() -> HotJournalFixture {
        let source_temp = tempfile::tempdir().unwrap();
        let source = source_temp.path().join("source.db");
        let writer = Connection::open(&source).unwrap();
        writer
            .execute_batch(
                "PRAGMA page_size = 512;
                 PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;
                 PRAGMA cache_size = 1;
                 PRAGMA cache_spill = 1;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);",
            )
            .unwrap();
        let value = "a".repeat(2048);
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        for id in 0..256_i64 {
            writer
                .execute("INSERT INTO entries VALUES (?1, ?2)", params![id, &value])
                .unwrap();
        }
        writer.execute_batch("COMMIT").unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .execute("UPDATE entries SET value = replace(value, 'a', 'b')", [])
            .unwrap();
        writer.cache_flush().unwrap();
        let journal = sidecar(&source, "-journal");
        let journal_bytes = fs::read(&journal).unwrap();
        assert!(journal_bytes.starts_with(&super::super::sqlite_observation::JOURNAL_MAGIC));

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("hot.db");
        fs::copy(&source, &db).unwrap();
        fs::write(sidecar(&db, "-journal"), journal_bytes).unwrap();
        writer.execute_batch("ROLLBACK").unwrap();

        HotJournalFixture { _temp: temp, db }
    }

    fn append_super_journal_trailer(journal: &Path, name: &[u8]) {
        let mut file = fs::OpenOptions::new().append(true).open(journal).unwrap();
        file.write_all(&1_048_577_u32.to_be_bytes()).unwrap();
        file.write_all(name).unwrap();
        file.write_all(&(name.len() as u32).to_be_bytes()).unwrap();
        let checksum = name.iter().fold(0_u32, |sum, byte| {
            sum.wrapping_add((*byte as i8 as i32) as u32)
        });
        file.write_all(&checksum.to_be_bytes()).unwrap();
        file.write_all(&super::super::sqlite_observation::JOURNAL_MAGIC)
            .unwrap();
        file.sync_all().unwrap();
    }

    #[cfg(unix)]
    fn native_path_bytes(path: &Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    fn native_path_bytes(path: &Path) -> Vec<u8> {
        path.to_str().unwrap().as_bytes().to_vec()
    }

    #[cfg(windows)]
    fn create_windows_junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .unwrap();
        assert!(status.success(), "failed to create a Windows junction");
    }

    fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        sidecar.into()
    }
}
