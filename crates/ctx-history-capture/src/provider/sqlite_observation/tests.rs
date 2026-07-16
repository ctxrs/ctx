mod tests {
    use std::{cell::Cell, fs, fs::FileTimes, rc::Rc, thread, time::Duration};

    use rusqlite::Connection;

    use crate::{install_disk_io_pacer, DiskIoPacer};

    use super::*;

    #[test]
    fn either_sqlite_header_version_byte_requires_wal_snapshot() {
        let mut header = [0_u8; SQLITE_HEADER_BYTES];
        header[..16].copy_from_slice(b"SQLite format 3\0");
        header[18] = 1;
        header[19] = 1;
        assert!(!main_header_uses_wal_mode(&header));
        header[18] = 2;
        assert!(main_header_uses_wal_mode(&header));
        header[18] = 1;
        header[19] = 2;
        assert!(main_header_uses_wal_mode(&header));
    }

    #[test]
    fn real_wal_validates_supported_page_sizes_and_both_checksum_orders() {
        for page_size in [512_u32, 65_536] {
            let fixture = real_wal_fixture(page_size);
            let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
            assert!(generation.requires_snapshot(), "page size {page_size}");
            let wal = generation.wal.as_ref().unwrap();
            assert!(wal.snapshot_len() <= wal.len());

            let alternate = fixture
                .temp
                .path()
                .join(format!("alternate-{page_size}.db"));
            fs::copy(&fixture.db, &alternate).unwrap();
            let mut wal_bytes = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
            let order = match be_u32(&wal_bytes[0..4]) {
                0x377f_0682 => WalChecksumOrder::BigEndian,
                0x377f_0683 => WalChecksumOrder::LittleEndian,
                magic => panic!("unexpected SQLite WAL magic {magic:#x}"),
            };
            rewrite_wal_checksum_order(&mut wal_bytes, order);
            fs::write(sidecar_path(&alternate, "-wal"), wal_bytes).unwrap();
            assert!(
                observe_sqlite_source_generation(&alternate)
                    .unwrap()
                    .requires_snapshot(),
                "alternate checksum order for page size {page_size}"
            );
        }
    }

    #[test]
    fn wal_validation_accounts_the_bytes_it_reads() {
        let fixture = real_wal_fixture(512);
        let pacer = DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = install_disk_io_pacer(pacer.clone());

        let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
        let committed_wal_bytes = generation.wal.as_ref().unwrap().snapshot_len();

        assert!(pacer.charged_bytes() >= committed_wal_bytes.saturating_mul(2));
    }

    #[test]
    fn real_wal_classifies_stable_corruption_stale_suffix_and_partial_frame() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        assert!(original.len() > WAL_HEADER_BYTES + WAL_FRAME_HEADER_BYTES);

        type WalCorruptionMutation = (&'static str, fn(&mut Vec<u8>));
        let corruptions: [WalCorruptionMutation; 4] = [
            ("header-checksum", |bytes: &mut Vec<u8>| bytes[24] ^= 0x01),
            ("salt", |bytes: &mut Vec<u8>| bytes[40] ^= 0x01),
            ("frame-checksum", |bytes: &mut Vec<u8>| bytes[48] ^= 0x01),
            ("partial-frame", |bytes: &mut Vec<u8>| {
                bytes.pop();
            }),
        ];
        for (label, mutate) in corruptions {
            let db = fixture.temp.path().join(format!("bad-{label}.db"));
            fs::copy(&fixture.db, &db).unwrap();
            let mut bytes = original.clone();
            mutate(&mut bytes);
            fs::write(sidecar_path(&db, "-wal"), bytes).unwrap();
            if label == "salt" {
                let generation = observe_sqlite_source_generation(&db).unwrap();
                assert!(generation.requires_snapshot(), "{label}");
                assert!(generation.deferred_reason().is_none(), "{label}");
            } else {
                let error = observe_sqlite_source_generation(&db).unwrap_err();
                let expected = if label == "partial-frame" {
                    io::ErrorKind::WouldBlock
                } else {
                    io::ErrorKind::InvalidData
                };
                assert_eq!(error.kind(), expected, "{label}");
            }
        }
    }

    #[test]
    fn transient_bad_wal_header_checksum_retries_when_the_generation_is_repaired() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        let db = fixture.temp.path().join("transient-header-checksum.db");
        fs::copy(&fixture.db, &db).unwrap();
        let wal = sidecar_path(&db, "-wal");
        let mut corrupt = original.clone();
        corrupt[24] ^= 0x01;
        fs::write(&wal, corrupt).unwrap();
        let wal_opens = Rc::new(Cell::new(0_usize));
        let wal_opens_for_hook = Rc::clone(&wal_opens);
        let wal_for_hook = wal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != wal_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            let opens = wal_opens_for_hook.get() + 1;
            wal_opens_for_hook.set(opens);
            if opens == 2 {
                fs::write(path, &original).unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(wal_opens.get() >= 3);
        assert!(generation.requires_snapshot());
    }

    #[test]
    fn stable_unsupported_wal_version_is_terminal() {
        let fixture = real_wal_fixture(512);
        let db = fixture.temp.path().join("unsupported-version.db");
        fs::copy(&fixture.db, &db).unwrap();
        let mut wal = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        rewrite_wal_format_version(&mut wal, WAL_FORMAT_VERSION + 1);
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();

        let error = observe_sqlite_source_generation(&db).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("format version"));
    }

    #[test]
    fn transient_unsupported_wal_version_retries_when_repaired() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        let db = fixture.temp.path().join("transient-version.db");
        fs::copy(&fixture.db, &db).unwrap();
        let wal = sidecar_path(&db, "-wal");
        let mut unsupported = original.clone();
        rewrite_wal_format_version(&mut unsupported, WAL_FORMAT_VERSION + 1);
        fs::write(&wal, unsupported).unwrap();
        let wal_opens = Rc::new(Cell::new(0_usize));
        let wal_opens_for_hook = Rc::clone(&wal_opens);
        let wal_for_hook = wal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != wal_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            let opens = wal_opens_for_hook.get() + 1;
            wal_opens_for_hook.set(opens);
            if opens == 2 {
                fs::write(path, &original).unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(wal_opens.get() >= 3);
        assert!(generation.requires_snapshot());
    }

    #[test]
    fn bad_frame_after_committed_wal_prefix_preserves_that_prefix() {
        let fixture = real_wal_fixture(512);
        let wal_path = sidecar_path(&fixture.db, "-wal");
        let committed_prefix_len = fs::metadata(&wal_path).unwrap().len();
        fixture
            .writer
            .execute("UPDATE entries SET value = 'sigma' WHERE id = 1", [])
            .unwrap();
        let mut wal = fs::read(&wal_path).unwrap();
        assert!(wal.len() as u64 > committed_prefix_len);
        wal[committed_prefix_len as usize + WAL_FRAME_HEADER_BYTES] ^= 0x01;

        let db = fixture.temp.path().join("valid-prefix.db");
        fs::copy(&fixture.db, &db).unwrap();
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();
        let generation = observe_sqlite_source_generation(&db).unwrap();
        let observed_wal = generation.wal.as_ref().unwrap();
        assert!(generation.requires_snapshot());
        assert_eq!(observed_wal.snapshot_len(), committed_prefix_len);
        assert!(generation.deferred_reason().is_none());
    }

    #[test]
    fn wal_reset_after_metadata_sampling_retries_instead_of_returning_unexpected_eof() {
        let WalFixture {
            temp: _temp,
            db,
            writer,
        } = real_wal_fixture(512);
        let reset = Rc::new(Cell::new(false));
        let reset_for_hook = Rc::clone(&reset);
        let wal = sidecar_path(&db, "-wal");
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == wal
                && phase == SqliteObservationTestPhase::BeforeWalFrameRead
                && !reset_for_hook.replace(true)
            {
                writer
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                    .unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(reset.get());
        assert!(generation.requires_snapshot());
    }

    #[test]
    fn journal_tail_truncation_after_metadata_sampling_retries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("tail-race.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        fs::write(&journal, real_hot_journal_bytes(512)).unwrap();
        let truncated = Rc::new(Cell::new(false));
        let truncated_for_hook = Rc::clone(&truncated);
        let journal_for_hook = journal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == journal_for_hook
                && phase == SqliteObservationTestPhase::BeforeJournalTailRead
                && !truncated_for_hook.replace(true)
            {
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(truncated.get());
        assert!(!generation.requires_snapshot());
    }

    #[test]
    fn journal_trailer_truncation_after_tail_sampling_retries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("trailer-race.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let super_journal = temp.path().join("trailer-race.db-mj");
        fs::write(&super_journal, b"active").unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, b"trailer-race.db-mj");
        fs::write(&journal, bytes).unwrap();
        let truncated = Rc::new(Cell::new(false));
        let truncated_for_hook = Rc::clone(&truncated);
        let journal_for_hook = journal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == journal_for_hook
                && phase == SqliteObservationTestPhase::BeforeJournalTrailerRead
                && !truncated_for_hook.replace(true)
            {
                let len = path.metadata().unwrap().len();
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_len(len - 8)
                    .unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(truncated.get());
        assert!(generation.requires_snapshot());
        assert!(generation.deferred_reason().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn transient_symlink_before_atomic_open_retries() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let held = temp.path().join("held.db");
        let outside = temp.path().join("outside.db");
        fs::write(&outside, b"outside").unwrap();
        let state = Rc::new(Cell::new(0_u8));
        let state_for_hook = Rc::clone(&state);
        let db_for_hook = db.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != db_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            match state_for_hook.get() {
                0 => {
                    fs::rename(path, &held).unwrap();
                    symlink(&outside, path).unwrap();
                    state_for_hook.set(1);
                }
                1 => {
                    fs::remove_file(path).unwrap();
                    fs::rename(&held, path).unwrap();
                    state_for_hook.set(2);
                }
                _ => {}
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(state.get(), 2);
        assert_eq!(generation.main().len(), 16);
    }

    #[test]
    fn transient_required_main_delete_create_gap_retries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("replace.db");
        let bytes = b"SQLite format 3\0".to_vec();
        fs::write(&db, &bytes).unwrap();
        let state = Rc::new(Cell::new(0_u8));
        let state_for_hook = Rc::clone(&state);
        let db_for_hook = db.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != db_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            match state_for_hook.get() {
                0 => {
                    fs::remove_file(path).unwrap();
                    state_for_hook.set(1);
                }
                1 => {
                    fs::write(path, &bytes).unwrap();
                    state_for_hook.set(2);
                }
                _ => {}
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(state.get(), 2);
        assert_eq!(generation.main().len(), 16);
    }

    #[test]
    fn stable_missing_required_main_preserves_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("missing.db");
        let opens = Rc::new(Cell::new(0_usize));
        let opens_for_hook = Rc::clone(&opens);
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == db && phase == SqliteObservationTestPhase::BeforeOpen {
                opens_for_hook.set(opens_for_hook.get() + 1);
            }
        });

        let error =
            observe_sqlite_source_generation(temp.path().join("missing.db").as_path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(opens.get(), SQLITE_GENERATION_MAX_ATTEMPTS);
    }

    #[cfg(unix)]
    #[test]
    fn stable_source_and_parent_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let db = real.join("source.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let file_link = temp.path().join("file-link.db");
        symlink(&db, &file_link).unwrap();
        let error = observe_sqlite_source_generation(&file_link).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let parent_link = temp.path().join("parent-link");
        symlink(&real, &parent_link).unwrap();
        let error = observe_sqlite_source_generation(&parent_link.join("source.db")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn stable_windows_leaf_and_parent_junctions_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let db = real.join("source.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();

        let junction = temp.path().join("junction");
        create_windows_junction(&junction, &real);
        let error = observe_sqlite_source_generation(&junction).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = observe_sqlite_source_generation(&junction.join("source.db")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        fs::remove_dir(&junction).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_component_walk_blocks_parent_swap() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let db = source.join("source.db");
        fs::write(&db, b"inside").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("source.db"), b"outside").unwrap();
        let held = temp.path().join("held-source");
        let blocked = Rc::new(Cell::new(false));
        let swapped = Rc::new(Cell::new(false));
        let blocked_for_hook = Rc::clone(&blocked);
        let swapped_for_hook = Rc::clone(&swapped);
        let source_for_hook = source.clone();
        let held_for_hook = held.clone();
        let outside_for_hook = outside.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if phase == SqliteObservationTestPhase::AfterParentOpen
                && path == source_for_hook
                && !swapped_for_hook.replace(true)
            {
                match fs::rename(path, &held_for_hook) {
                    Ok(()) => create_windows_junction(path, &outside_for_hook),
                    Err(error)
                        if error.kind() == io::ErrorKind::PermissionDenied
                            || matches!(error.raw_os_error(), Some(5 | 32)) =>
                    {
                        swapped_for_hook.set(false);
                        blocked_for_hook.set(true);
                    }
                    Err(error) => panic!("unexpected parent swap failure: {error}"),
                }
            }
        });

        let mut opened = open_source_file_no_follow(&db).unwrap();
        let mut contents = String::new();
        opened.file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "inside");
        assert!(blocked.get());
        drop(opened);
        if swapped.get() {
            fs::remove_dir(&source).unwrap();
            fs::rename(&held, &source).unwrap();
        }
    }

    #[test]
    fn wal_page_size_one_is_invalid() {
        let fixture = real_wal_fixture(512);
        let db = fixture.temp.path().join("page-size-one.db");
        fs::copy(&fixture.db, &db).unwrap();
        let mut wal = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        wal[8..12].copy_from_slice(&1_u32.to_be_bytes());
        let order = match be_u32(&wal[0..4]) {
            0x377f_0682 => WalChecksumOrder::LittleEndian,
            0x377f_0683 => WalChecksumOrder::BigEndian,
            _ => unreachable!(),
        };
        let checksum = wal_checksum(order, &wal[..24], [0, 0]);
        wal[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        wal[28..32].copy_from_slice(&checksum[1].to_be_bytes());
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();

        assert!(observe_sqlite_source_generation(&db)
            .unwrap()
            .requires_snapshot());
    }

    #[test]
    fn wal_valid_prefix_ceiling_is_retryable() {
        let error =
            wal_frame_end_within_snapshot_ceiling(SQLITE_SNAPSHOT_MAX_BYTES, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), WAL_RESOURCE_REASON);
    }

    #[test]
    fn wal_restart_ignores_stale_physical_frames_after_the_valid_prefix() {
        let fixture = real_wal_fixture(512);
        fixture
            .writer
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE extra (id INTEGER PRIMARY KEY, value BLOB);
                 INSERT INTO extra(value) VALUES (zeroblob(262144));
                 COMMIT;",
            )
            .unwrap();
        let wal_path = sidecar_path(&fixture.db, "-wal");
        let old_wal = fs::read(&wal_path).unwrap();
        fixture
            .writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        fixture
            .writer
            .execute("UPDATE entries SET value = 'sigma' WHERE id = 1", [])
            .unwrap();
        let restarted_wal = fs::read(&wal_path).unwrap();
        assert!(restarted_wal.len() < old_wal.len());
        let mut reused_wal = restarted_wal.clone();
        reused_wal.extend_from_slice(&old_wal[restarted_wal.len()..]);
        fs::write(&wal_path, reused_wal).unwrap();

        let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
        let wal = generation.wal.as_ref().unwrap();
        assert!(generation.requires_snapshot());
        assert_eq!(wal.len(), old_wal.len() as u64);
        assert_eq!(wal.snapshot_len(), restarted_wal.len() as u64);
        assert!(wal.snapshot_len() < wal.len());
    }

    #[test]
    fn rollback_journal_modes_leave_no_hot_generation_after_commit() {
        for mode in ["DELETE", "TRUNCATE", "PERSIST"] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join(format!("{mode}.db"));
            let conn = Connection::open(&db).unwrap();
            let actual: String = conn
                .query_row(&format!("PRAGMA journal_mode = {mode}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(actual.to_uppercase(), mode);
            conn.execute_batch(
                "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'committed');",
            )
            .unwrap();

            let generation = observe_sqlite_source_generation(&db).unwrap();
            assert!(!generation.requires_snapshot(), "journal mode {mode}");
            assert!(
                generation.deferred_reason().is_none(),
                "journal mode {mode}"
            );
        }
    }

    #[test]
    fn super_journal_presence_controls_hot_child_journal_state() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("attached-main.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let super_journal_name = b"attached-main.db-mj H8a1";
        let super_journal = temp.path().join("attached-main.db-mj H8a1");
        fs::write(&super_journal, b"active multi-database commit").unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, super_journal_name);
        fs::write(&journal, bytes).unwrap();

        let present = observe_sqlite_source_generation(&db).unwrap();
        assert!(!present.requires_snapshot());
        assert_eq!(present.deferred_reason(), Some(SUPER_JOURNAL_REASON));

        fs::remove_file(super_journal).unwrap();
        let missing = observe_sqlite_source_generation(&db).unwrap();
        assert!(!missing.requires_snapshot());
        assert!(missing.deferred_reason().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn super_journal_uses_non_utf8_native_relative_path_without_loss() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("attached-main.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let name = b"attached-main.db-mj-\x80";
        fs::write(
            temp.path().join(OsString::from_vec(name.to_vec())),
            b"active",
        )
        .unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, name);
        fs::write(journal, bytes).unwrap();

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(generation.deferred_reason(), Some(SUPER_JOURNAL_REASON));
    }

    #[test]
    fn same_stat_checkpointed_update_changes_main_file_identity() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("same-stat.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
             INSERT INTO entries VALUES (1, 'alpha');
             PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        let before_metadata = fs::metadata(&db).unwrap();
        let before_modified = before_metadata.modified().unwrap();
        let before_header = fs::read(&db).unwrap()[..SQLITE_HEADER_BYTES].to_vec();
        let before = observe_sqlite_source_generation(&db).unwrap();
        thread::sleep(Duration::from_millis(2));

        conn.execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        File::options()
            .write(true)
            .open(&db)
            .unwrap()
            .set_times(FileTimes::new().set_modified(before_modified))
            .unwrap();

        let after_metadata = fs::metadata(&db).unwrap();
        assert_eq!(after_metadata.len(), before_metadata.len());
        assert_eq!(after_metadata.modified().unwrap(), before_modified);
        assert_eq!(
            &fs::read(&db).unwrap()[..SQLITE_HEADER_BYTES],
            before_header
        );
        let after = observe_sqlite_source_generation(&db).unwrap();
        assert_ne!(before.main().sentinel(), after.main().sentinel());
    }

    struct WalFixture {
        temp: tempfile::TempDir,
        db: PathBuf,
        writer: Connection,
    }

    fn real_wal_fixture(page_size: u32) -> WalFixture {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(format!("wal-{page_size}.db"));
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch(&format!(
                "PRAGMA page_size = {page_size};
                 VACUUM;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);"
            ))
            .unwrap();
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        assert!(sidecar_path(&db, "-wal").is_file());
        WalFixture { temp, db, writer }
    }

    fn rewrite_wal_checksum_order(bytes: &mut [u8], order: WalChecksumOrder) {
        let magic = match order {
            WalChecksumOrder::LittleEndian => 0x377f_0682_u32,
            WalChecksumOrder::BigEndian => 0x377f_0683_u32,
        };
        bytes[0..4].copy_from_slice(&magic.to_be_bytes());
        let mut checksum = wal_checksum(order, &bytes[..24], [0, 0]);
        bytes[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        bytes[28..32].copy_from_slice(&checksum[1].to_be_bytes());
        let page_size = be_u32(&bytes[8..12]) as usize;
        let frame_size = WAL_FRAME_HEADER_BYTES + page_size;
        for frame in bytes[WAL_HEADER_BYTES..].chunks_exact_mut(frame_size) {
            checksum = wal_checksum(order, &frame[..8], checksum);
            checksum = wal_checksum(order, &frame[WAL_FRAME_HEADER_BYTES..], checksum);
            frame[16..20].copy_from_slice(&checksum[0].to_be_bytes());
            frame[20..24].copy_from_slice(&checksum[1].to_be_bytes());
        }
    }

    fn rewrite_wal_format_version(bytes: &mut [u8], version: u32) {
        bytes[4..8].copy_from_slice(&version.to_be_bytes());
        let order = match be_u32(&bytes[0..4]) {
            0x377f_0682 => WalChecksumOrder::LittleEndian,
            0x377f_0683 => WalChecksumOrder::BigEndian,
            magic => panic!("unexpected SQLite WAL magic {magic:#x}"),
        };
        let checksum = wal_checksum(order, &bytes[..24], [0, 0]);
        bytes[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        bytes[28..32].copy_from_slice(&checksum[1].to_be_bytes());
    }

    fn real_hot_journal_bytes(page_size: u32) -> Vec<u8> {
        let sector_size = 512_u32;
        let mut bytes = vec![0_u8; sector_size as usize + page_size as usize + 8];
        bytes[..8].copy_from_slice(&JOURNAL_MAGIC);
        bytes[8..12].copy_from_slice(&1_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
        bytes[16..20].copy_from_slice(&1_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&sector_size.to_be_bytes());
        bytes[24..28].copy_from_slice(&page_size.to_be_bytes());
        bytes[sector_size as usize..sector_size as usize + 4].copy_from_slice(&1_u32.to_be_bytes());
        bytes
    }

    fn append_super_journal_trailer(journal: &mut Vec<u8>, name: &[u8]) {
        journal.extend_from_slice(&1_048_577_u32.to_be_bytes());
        journal.extend_from_slice(name);
        journal.extend_from_slice(&(name.len() as u32).to_be_bytes());
        let checksum = name.iter().fold(0_u32, |sum, byte| {
            sum.wrapping_add((*byte as i8 as i32) as u32)
        });
        journal.extend_from_slice(&checksum.to_be_bytes());
        journal.extend_from_slice(&JOURNAL_MAGIC);
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
}
