use super::*;
use std::{cell::RefCell, os::unix::fs::FileExt, sync::mpsc, thread};

type SyncFn = unsafe extern "C" fn(*mut ffi::sqlite3_file, i32) -> i32;
struct SyncPause {
    original: SyncFn,
    arrived: Option<mpsc::Sender<()>>,
    resume: mpsc::Receiver<()>,
}
thread_local! {
    static SYNC_PAUSE: RefCell<Option<SyncPause>> = const { RefCell::new(None) };
}

unsafe extern "C" fn pause_after_sync(file: *mut ffi::sqlite3_file, flags: i32) -> i32 {
    SYNC_PAUSE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(pause) = slot.as_mut() else {
            return ffi::SQLITE_IOERR;
        };
        let rc = unsafe { (pause.original)(file, flags) };
        if rc != ffi::SQLITE_OK {
            return rc;
        }
        if let Some(arrived) = pause.arrived.take() {
            if arrived.send(()).is_err()
                || pause.resume.recv_timeout(Duration::from_secs(30)).is_err()
            {
                return ffi::SQLITE_IOERR;
            }
        }
        rc
    })
}

// Always release the writer, including when a parent assertion panics.
struct Resume(mpsc::Sender<()>);
impl Drop for Resume {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn pause_commit(writer: Connection) -> (Resume, thread::JoinHandle<Connection>) {
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        writer
            .execute_batch(
                "PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0;
            BEGIN IMMEDIATE; UPDATE messages SET body='committed edit';",
            )
            .unwrap();
        let mut wal: *mut ffi::sqlite3_file = std::ptr::null_mut();
        // Patch only this fixture connection's WAL xSync. No global VFS or
        // production hooks. Stock SQLite publishes walIndexWriteHdr after sync.
        unsafe {
            assert_eq!(
                ffi::sqlite3_file_control(
                    writer.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_JOURNAL_POINTER,
                    (&mut wal as *mut *mut ffi::sqlite3_file).cast()
                ),
                ffi::SQLITE_OK
            );
            assert!(!wal.is_null());
            let original = (*wal).pMethods;
            assert!(
                ((*original).xDeviceCharacteristics.unwrap())(wal)
                    & ffi::SQLITE_IOCAP_POWERSAFE_OVERWRITE
                    != 0,
                "fixture requires no post-sync sector-padding write"
            );
            let mut methods = Box::new(*original);
            SYNC_PAUSE.with(|slot| {
                *slot.borrow_mut() = Some(SyncPause {
                    original: methods.xSync.unwrap(),
                    arrived: Some(arrived_tx),
                    resume: resume_rx,
                })
            });
            methods.xSync = Some(pause_after_sync);
            (*wal).pMethods = &*methods;
            let result = writer.execute_batch("COMMIT");
            (*wal).pMethods = original;
            SYNC_PAUSE.with(|slot| *slot.borrow_mut() = None);
            result.unwrap();
        }
        writer
    });
    arrived_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    (Resume(resume_tx), worker)
}

fn selected(authority: &SqliteSourceDirectoryAuthority) -> SqliteSourceReadSnapshot {
    authority
        .open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["messages"],
            |_| Ok::<_, ()>(()),
        )
        .unwrap()
}

#[test]
fn header_only_commit_publication_invalidates_persisted_selective_replay() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let writer = create_persistent_wal(&database);
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let (resume, writer) = pause_commit(writer);
    let before_db = fs::read(&database).unwrap();
    let before_wal = fs::read(temp.path().join("provider.sqlite-wal")).unwrap();
    let family =
        SqliteSourceFamily::open(&authority, OsStr::new("provider.sqlite"), || {}).unwrap();
    let native = family.capture_revision_evidence().unwrap();
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let snapshot = selected(&authority);
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert!(snapshot.admitted_revision_is_replay_safe());
    assert_eq!(snapshot.evidence().physical_revision(), fence.revision());
    snapshot.finish().unwrap();
    fence.revalidate().unwrap();

    // A recovered full-family copy can already see the commit marker, unlike
    // the live reader. It must not publish replay authority for the old header.
    let recovered = authority
        .open_stable_snapshot(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(read_values(&recovered), ["committed edit"]);
    assert!(!recovered.admitted_revision_is_replay_safe());
    assert_ne!(recovered.evidence().physical_revision(), fence.revision());
    recovered.finish().unwrap();

    drop(resume);
    let _writer = writer.join().unwrap();
    assert_eq!(fs::read(&database).unwrap(), before_db);
    assert_eq!(
        fs::read(temp.path().join("provider.sqlite-wal")).unwrap(),
        before_wal
    );
    let published = family.capture_revision_evidence().unwrap();
    assert_eq!(native.database, published.database);
    assert_eq!(native.wal, published.wal);
    assert!(fence.revalidate().unwrap_err().is_source_changed());
    let next = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_ne!(next.revision(), fence.revision());
    let snapshot = selected(&authority);
    assert_eq!(read_values(&snapshot), ["committed edit"]);
    assert!(snapshot.admitted_revision_is_replay_safe());
    assert_eq!(snapshot.evidence().physical_revision(), next.revision());
    snapshot.finish().unwrap();
    next.revalidate().unwrap();
}

#[test]
fn header_publication_during_selective_capture_withholds_replay_authority() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let writer = create_persistent_wal(&temp.path().join("provider.sqlite"));
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let (resume, writer) = pause_commit(writer);
    let mut resume = Some(resume);
    let mut writer = Some(writer);
    let mut committed_writer = None;
    let snapshot = authority
        .open_selected_tables_snapshot_with_progress(
            OsStr::new("provider.sqlite"),
            &["messages"],
            |_| {
                // Progress starts after pinning the source transaction.
                if let Some(resume) = resume.take() {
                    drop(resume);
                    committed_writer = Some(writer.take().unwrap().join().unwrap());
                }
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    assert!(committed_writer.is_some());
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    assert!(!snapshot.admitted_revision_is_replay_safe());
    let next = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_ne!(snapshot.evidence().physical_revision(), next.revision());
    snapshot.finish().unwrap();
}

#[test]
fn ordinary_reader_marks_do_not_invalidate_committed_replay() {
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    let shm = temp.path().join("provider.sqlite-shm");
    let before = fs::read(&shm).unwrap();
    let reader = Connection::open(&database).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    assert_eq!(
        reader
            .query_row("SELECT body FROM messages", [], |row| row
                .get::<_, String>(0))
            .unwrap(),
        "from-wal"
    );
    let after = fs::read(&shm).unwrap();
    assert_eq!(&before[..96], &after[..96]);
    assert_ne!(
        &before[100..120],
        &after[100..120],
        "real reader must update a read mark"
    );
    fence.revalidate().unwrap();
    assert_eq!(
        fence.revision(),
        authority
            .observe_replay_fence(OsStr::new("provider.sqlite"))
            .unwrap()
            .revision()
    );
    assert_eq!(authority.snapshot_counters(), Default::default());
}

#[test]
fn pinned_live_wal_view_retains_replay_when_commit_header_is_unchanged() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let _writer = create_persistent_wal(&temp.path().join("provider.sqlite"));
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let snapshot = authority
        .open_incremental_snapshot_with_progress(OsStr::new("provider.sqlite"), |_| Ok::<_, ()>(()))
        .unwrap();
    assert_eq!(
        snapshot.strategy(),
        SqliteSourceSnapshotStrategy::PinnedReadOnlyWal
    );
    assert!(snapshot.admitted_revision_is_replay_safe());
    assert_eq!(read_values(&snapshot), ["from-wal"]);
    let fence = authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap();
    assert_eq!(snapshot.evidence().physical_revision(), fence.revision());
    snapshot.finish().unwrap();
    fence.revalidate().unwrap();
    assert_eq!(authority.snapshot_counters().source_bytes_copied(), 0);
}

#[test]
fn unavailable_wal_headers_deny_replay_but_allow_private_capture() {
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let database = temp.path().join("provider.sqlite");
    let _writer = create_persistent_wal(&database);
    let authority = retain_parent_in_data_root(data.path(), temp.path());
    let shm = temp.path().join("provider.sqlite-shm");
    let bytes = fs::read(&shm).unwrap();
    let file = File::options().write(true).open(&shm).unwrap();
    let mut invalid = Vec::new();
    let mut torn = bytes[..96].to_vec();
    torn[8] ^= 1;
    invalid.push(torn);
    let mut checksum = bytes[..96].to_vec();
    checksum[40] ^= 1;
    checksum[88] ^= 1;
    invalid.push(checksum);
    let mut version = bytes[..96].to_vec();
    version[..4].copy_from_slice(&1_u32.to_ne_bytes());
    version[48..52].copy_from_slice(&1_u32.to_ne_bytes());
    invalid.push(version);
    invalid.push(vec![0; 96]);
    for header in invalid {
        file.write_all_at(&header, 0).unwrap();
        assert!(authority
            .observe_replay_fence(OsStr::new("provider.sqlite"))
            .unwrap_err()
            .is_source_changed());
        let snapshot = authority
            .open_stable_snapshot(OsStr::new("provider.sqlite"))
            .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);
        assert!(!snapshot.admitted_revision_is_replay_safe());
        snapshot.finish().unwrap();
    }
    file.write_all_at(&bytes[..96], 0).unwrap();
    authority
        .observe_replay_fence(OsStr::new("provider.sqlite"))
        .unwrap()
        .revalidate()
        .unwrap();
}
