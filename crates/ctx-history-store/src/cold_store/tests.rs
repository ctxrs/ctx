use chrono::{TimeZone, Utc};
use ctx_history_core::HistoryRecord;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use uuid::Uuid;

use super::*;

#[test]
fn target_created_before_no_replace_install_preserves_target_and_stage() {
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("stage.sqlite");
    let target = temp.path().join("work.sqlite");
    fs::write(&stage, b"stage").unwrap();
    fs::write(&target, b"winner").unwrap();

    assert!(matches!(
        install_same_filesystem(&stage, &target),
        Err(StoreError::ColdStoreTargetChanged(path)) if path == target
    ));
    assert_eq!(fs::read(&target).unwrap(), b"winner");
    assert_eq!(fs::read(&stage).unwrap(), b"stage");
}

#[test]
fn no_replace_install_publishes_the_exact_stage_object() {
    let temp = tempfile::tempdir().unwrap();
    let stage = temp.path().join("stage.sqlite");
    let target = temp.path().join("work.sqlite");
    fs::write(&stage, b"stage").unwrap();
    let stage_identity = Handle::from_path(&stage).unwrap();

    install_same_filesystem(&stage, &target).unwrap();

    assert_eq!(Handle::from_path(&target).unwrap(), stage_identity);
    assert_eq!(fs::read(&target).unwrap(), b"stage");
    assert_eq!(fs::read(&stage).unwrap(), b"stage");
}

#[test]
fn cold_stage_open_does_not_migrate_parent_legacy_store() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = temp.path().join("work-record").join("work.sqlite");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, b"legacy-store-canary").unwrap();
    let target = temp.path().join("work.sqlite");

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();

    assert!(!target.exists());
    assert_eq!(fs::read(&legacy).unwrap(), b"legacy-store-canary");
    drop(builder);
    assert_eq!(fs::read(&legacy).unwrap(), b"legacy-store-canary");
}

#[test]
fn cold_load_retains_every_canonical_explicit_index() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    let temp_store = builder
        .store()
        .unwrap()
        .conn
        .query_row("PRAGMA temp_store", [], |row| row.get::<_, i64>(0))
        .unwrap();
    assert_eq!(temp_store, 1, "cold scratch storage must be disk-backed");
    let during_load = query_count(
        &builder.store().unwrap().conn,
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND sql IS NOT NULL",
    )
    .unwrap();
    assert!(during_load > 0);

    let receipt = builder.finish().unwrap();

    assert_eq!(receipt.deferred_index_count, 0);
    let reopened = Store::open_read_only(target).unwrap();
    let installed = query_count(
        &reopened.conn,
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND sql IS NOT NULL",
    )
    .unwrap();
    assert_eq!(installed, during_load);
}

#[test]
fn cold_lock_owner_removes_only_exact_orphan_stage_names() {
    let temp = tempfile::tempdir().unwrap();
    let target_name = std::ffi::OsStr::new("work.sqlite");
    let uuid = "9ff4ee19-a3bf-4b8b-81ce-0b768335cfac";
    let stage = temp
        .path()
        .join(format!("work.sqlite{COLD_STAGE_MARKER}{uuid}.sqlite"));
    let sidecar = append_suffix(&stage, "-wal");
    let impostor = temp.path().join(format!(
        "work.sqlite{COLD_STAGE_MARKER}{uuid}.sqlite.backup"
    ));
    fs::write(&stage, b"orphan").unwrap();
    fs::write(&sidecar, b"orphan-sidecar").unwrap();
    fs::write(&impostor, b"keep").unwrap();

    cleanup_orphaned_stage_files(temp.path(), target_name).unwrap();

    assert!(!stage.exists());
    assert!(!sidecar.exists());
    assert_eq!(fs::read(impostor).unwrap(), b"keep");
}

fn control_record(title: &str) -> HistoryRecord {
    HistoryRecord {
        id: Uuid::new_v4(),
        title: title.to_owned(),
        body: "carried control record".to_owned(),
        tags: vec!["agent-history".to_owned()],
        kind: "agent_history".to_owned(),
        workspace: Some("/workspace".to_owned()),
        created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        updated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
    }
}

fn adjacent_cold_names(parent: &Path) -> Vec<PathBuf> {
    fs::read_dir(parent)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(COLD_STAGE_MARKER))
        })
        .collect()
}

#[test]
fn existing_empty_generation_is_rebuilt_and_carries_its_control_records() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = control_record("codex agent history");
    let existing = Store::open(&target).unwrap();
    existing.upsert_record(&carried).unwrap();
    drop(existing);
    let retired_identity = Handle::from_path(&target).unwrap();

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    assert_eq!(builder.counts().unwrap().history_records, 1);
    let receipt = builder.finish().unwrap();

    assert_eq!(receipt.counts.history_records, 1);
    assert_ne!(Handle::from_path(&target).unwrap(), retired_identity);
    let installed = Store::open_read_only(&target).unwrap();
    assert_eq!(
        installed.get_record(carried.id).unwrap().title,
        carried.title
    );
    assert_eq!(
        installed.search_records("codex", 10).unwrap().len(),
        1,
        "carried records must be searchable in the published generation"
    );
    drop(installed);
    assert!(adjacent_cold_names(temp.path()).is_empty());
}

#[test]
fn existing_generation_with_content_stays_on_the_ordinary_writer() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let existing = Store::open(&target).unwrap();
    existing
        .conn
        .execute_batch(
            "INSERT INTO catalog_sessions
             (source_path, provider, source_format, source_root, agent_type,
              file_size_bytes, file_modified_at_ms, cataloged_at_ms)
             VALUES ('/root/a.jsonl', 'codex', 'codex_session_jsonl_tree', '/root',
                     'primary', 1, 1, 1)",
        )
        .unwrap();
    drop(existing);
    let identity = Handle::from_path(&target).unwrap();

    assert!(ColdStoreBuild::begin(&target).unwrap().is_none());

    assert_eq!(Handle::from_path(&target).unwrap(), identity);
    let unchanged = Store::open_read_only(&target).unwrap();
    assert_eq!(
        query_count(&unchanged.conn, "SELECT COUNT(*) FROM catalog_sessions").unwrap(),
        1
    );
    drop(unchanged);
    assert!(adjacent_cold_names(temp.path()).is_empty());
}

/// A destination that another owner recreated between admission and
/// publication is never clobbered, even though it is byte-identical in size
/// to the empty generation this build admitted.
#[test]
fn concurrently_recreated_target_is_never_clobbered() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    drop(Store::open(&target).unwrap());

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    let raced = target.clone();
    let result = builder.finish_with_pre_install(move |_| {
        // A different owner replaces the admitted object under the same name.
        fs::remove_file(&raced)?;
        fs::write(&raced, b"concurrent-owner")?;
        Ok(())
    });

    assert!(matches!(
        result,
        Err(StoreError::ColdStoreTargetChanged(path)) if path == target
    ));
    assert_eq!(fs::read(&target).unwrap(), b"concurrent-owner");
    assert!(adjacent_cold_names(temp.path()).is_empty());
}

/// A writer that lands content in the admitted generation between admission
/// and publication is observed by the pre-install proof, and its rows
/// survive.
#[test]
fn target_that_gains_content_before_install_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    drop(Store::open(&target).unwrap());
    let identity = Handle::from_path(&target).unwrap();

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    let raced = target.clone();
    let result = builder.finish_with_pre_install(move |_| {
        let concurrent = Store::open(&raced)?;
        concurrent.upsert_record(&control_record("concurrent import"))?;
        Ok(())
    });

    assert!(matches!(
        result,
        Err(StoreError::ColdStoreTargetChanged(path)) if path == target
    ));
    assert_eq!(Handle::from_path(&target).unwrap(), identity);
    let preserved = Store::open_read_only(&target).unwrap();
    assert_eq!(
        query_count(&preserved.conn, "SELECT COUNT(*) FROM history_records").unwrap(),
        1
    );
    drop(preserved);
    assert!(adjacent_cold_names(temp.path()).is_empty());
}

/// A build interrupted after the old generation is unlinked leaves the
/// retired object adjacent, and the next lock owner puts it back.
#[test]
fn retired_generation_is_restored_after_an_interrupted_install() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = control_record("codex agent history");
    let existing = Store::open(&target).unwrap();
    existing.upsert_record(&carried).unwrap();
    drop(existing);
    let identity = Handle::from_path(&target).unwrap();

    // Reproduce the state a crash between unlink and publish leaves behind.
    let retired = adjacent_retired_path(&target);
    fs::hard_link(&target, &retired).unwrap();
    fs::remove_file(&target).unwrap();
    assert!(!target.exists());

    assert!(
        restore_retired_target(temp.path(), std::ffi::OsStr::new("work.sqlite"), &target).unwrap()
    );

    assert_eq!(Handle::from_path(&target).unwrap(), identity);
    let restored = Store::open_read_only(&target).unwrap();
    assert_eq!(
        restored.get_record(carried.id).unwrap().title,
        carried.title
    );
    drop(restored);
    assert!(!retired.exists());
    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    assert_eq!(builder.counts().unwrap().history_records, 1);
}

/// A crash between creating the durable backup link and unlinking the original
/// leaves two names for one live object. The backup is redundant, so the next
/// lock owner drops it and the destination stays admissible.
#[test]
fn interrupted_backup_link_leaves_a_redundant_name_the_next_owner_drops() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = control_record("codex agent history");
    let existing = Store::open(&target).unwrap();
    existing.upsert_record(&carried).unwrap();
    drop(existing);
    let identity = Handle::from_path(&target).unwrap();

    // Reproduce the state a crash between link and unlink leaves behind.
    let retired = adjacent_retired_path(&target);
    fs::hard_link(&target, &retired).unwrap();

    assert!(
        restore_retired_target(temp.path(), std::ffi::OsStr::new("work.sqlite"), &target).unwrap()
    );

    assert!(!retired.exists());
    assert_eq!(Handle::from_path(&target).unwrap(), identity);
    let preserved = Store::open_read_only(&target).unwrap();
    assert_eq!(
        preserved.get_record(carried.id).unwrap().title,
        carried.title
    );
}

/// A retired generation that is *not* the live destination is real data nothing
/// else will claim. It is retained, never swept, and the rebuild declines until
/// it is resolved.
#[test]
fn a_retired_generation_that_differs_from_the_target_is_retained_and_declines() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    drop(Store::open(&target).unwrap());
    let retired = adjacent_retired_path(&target);
    fs::write(&retired, b"retired-generation").unwrap();

    assert!(
        !restore_retired_target(temp.path(), std::ffi::OsStr::new("work.sqlite"), &target).unwrap()
    );
    assert_eq!(fs::read(&retired).unwrap(), b"retired-generation");

    // The orphan sweep must never claim it either.
    cleanup_orphaned_stage_files(temp.path(), std::ffi::OsStr::new("work.sqlite")).unwrap();
    assert_eq!(fs::read(&retired).unwrap(), b"retired-generation");

    assert!(ColdStoreBuild::begin(&target).unwrap().is_none());
    assert_eq!(fs::read(&retired).unwrap(), b"retired-generation");
}

#[test]
fn a_retained_retired_generation_is_named_ahead_of_its_cause() {
    let retained = PathBuf::from("/data/work.sqlite.ctx-native-cold-x.retired.sqlite");
    let error = retained_generation_error(
        Some(retained.clone()),
        StoreError::ColdStoreTargetChanged(PathBuf::from("/data/work.sqlite")),
    );

    assert!(matches!(
        &error,
        StoreError::ColdStoreRetiredGenerationRetained { path, .. } if *path == retained
    ));
    assert!(error.to_string().contains("cold Store target changed"));
    assert!(matches!(
        retained_generation_error(None, StoreError::ColdStoreInvalidState),
        StoreError::ColdStoreInvalidState
    ));
}

/// The invariant that makes the emptiness proof hold through publication: while
/// the exclusive publication lease is held, no writable Store can be opened, so
/// no commit can reach the destination between the proof and the install.
#[test]
fn a_writable_store_cannot_be_opened_while_publication_holds_the_lease() {
    use std::sync::mpsc;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    drop(Store::open(&target).unwrap());

    let lease = acquire_publication_lease(&target, Duration::ZERO).unwrap();
    let (ready, opened) = (mpsc::channel(), mpsc::channel());
    let writer_target = target.clone();
    let writer = std::thread::spawn(move || {
        ready.0.send(()).unwrap();
        let store = Store::open(&writer_target).unwrap();
        opened.0.send(()).unwrap();
        store.upsert_record(&control_record("raced write")).unwrap();
    });
    ready.1.recv().unwrap();

    assert!(
        matches!(
            opened.1.recv_timeout(Duration::from_millis(750)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "a writable Store must not open while publication owns the lease"
    );

    drop(lease);
    opened
        .1
        .recv_timeout(Duration::from_secs(10))
        .expect("the writer must proceed once the lease is released");
    writer.join().unwrap();
    let store = Store::open_read_only(&target).unwrap();
    assert_eq!(
        query_count(&store.conn, "SELECT COUNT(*) FROM history_records").unwrap(),
        1
    );
}

/// Pins the exact window the defect lived in: a writer that starts *after* the
/// emptiness proof and *before* the install. It must not be able to commit
/// there, and its write must land in the published generation instead of being
/// replaced away.
#[test]
fn a_writer_starting_between_the_proof_and_the_install_cannot_commit_into_the_gap() {
    use std::sync::{mpsc, Arc, Mutex};

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = control_record("codex agent history");
    let existing = Store::open(&target).unwrap();
    existing.upsert_record(&carried).unwrap();
    drop(existing);

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    let raced = control_record("post-proof write");
    let record = raced.clone();
    let writer_target = target.clone();
    let joiner: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::new(Mutex::new(None));
    let hook_joiner = Arc::clone(&joiner);

    set_post_proof_hook(Box::new(move || {
        let (done, wait) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let store = Store::open(&writer_target).unwrap();
            store.upsert_record(&record).unwrap();
            let _ = done.send(());
        });
        // The lease is held here. The writer must still be blocked in
        // `Store::open`; if it completed, a commit reached the destination
        // inside the window the proof is supposed to cover.
        assert!(
            matches!(
                wait.recv_timeout(Duration::from_millis(400)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a write committed between the emptiness proof and the install"
        );
        *hook_joiner.lock().unwrap() = Some(handle);
    }));

    builder.finish().unwrap();
    let handle = joiner.lock().unwrap().take().expect("hook must have run");
    handle.join().unwrap();

    let store = Store::open_read_only(&target).unwrap();
    assert!(
        store.get_record(raced.id).is_ok(),
        "the raced write must land in the published generation"
    );
    assert!(
        store.get_record(carried.id).is_ok(),
        "the carried control record must survive publication"
    );
    assert_eq!(
        query_count(&store.conn, "SELECT COUNT(*) FROM history_records").unwrap(),
        2
    );
}

/// The case the defect actually lost data in: a Store opened *before* the
/// emptiness proof commits *after* it, inside the window publication is about
/// to replace. Holding the lease from the proof through the install makes that
/// window unreachable — publication cannot even start while the writer holds
/// the destination open, so the commit either never happens here or is seen by
/// the proof. It is never replaced away.
#[test]
fn a_commit_from_a_store_opened_before_the_proof_is_never_replaced_away() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    set_publication_lease_wait(Duration::from_millis(300));
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let carried = control_record("codex agent history");
    let existing = Store::open(&target).unwrap();
    existing.upsert_record(&carried).unwrap();
    drop(existing);
    let identity = Handle::from_path(&target).unwrap();

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
    // Opened before the proof, still live when publication wants the lease.
    let holder = Store::open(&target).unwrap();
    let raced = control_record("post-proof commit");
    let record = raced.clone();
    let committed = Arc::new(AtomicBool::new(false));
    let hook_committed = Arc::clone(&committed);
    set_post_proof_hook(Box::new(move || {
        holder.upsert_record(&record).unwrap();
        hook_committed.store(true, Ordering::SeqCst);
    }));

    let published = builder.finish();

    let store = Store::open_read_only(&target).unwrap();
    if committed.load(Ordering::SeqCst) {
        assert!(
            store.get_record(raced.id).is_ok(),
            "a commit inside the protected window was replaced away"
        );
    } else {
        assert!(
            published.is_err(),
            "publication must not proceed while a writer holds the destination"
        );
        assert_eq!(Handle::from_path(&target).unwrap(), identity);
        assert!(store.get_record(carried.id).is_ok());
    }
}

/// An advisory lock outlives the liveness of its holder: a stopped process, a
/// frozen cgroup or a stalled filesystem keeps the lease held. A writable open
/// must therefore give up in bounded time with an error that names the lease,
/// rather than hanging the whole product behind one stuck publication.
#[test]
fn a_stuck_publication_fails_a_writable_open_in_bounded_time() {
    crate::connection::set_shared_publication_lease_wait(Duration::from_millis(200));
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    drop(Store::open(&target).unwrap());

    // Held past the open deadline, exactly as a frozen publisher would.
    let lease = acquire_publication_lease(&target, Duration::ZERO).unwrap();
    let started = Instant::now();
    let blocked = Store::open(&target);
    let elapsed = started.elapsed();

    let error = match blocked {
        Err(error) => error,
        Ok(_) => panic!("a writable open must not succeed while the lease is held"),
    };
    assert!(
        matches!(
            &error,
            StoreError::StorePublicationLeaseUnavailable(path)
                if *path == crate::connection::publication_lease_path(&target)
        ),
        "expected a named stuck-publication error, got {error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the open must fail in bounded time, took {elapsed:?}"
    );
    let message = error.to_string();
    assert!(message.contains("publication appears stuck"));
    assert!(message.contains(".ctx-store-publication.lock"));

    drop(lease);
    Store::open(&target).expect("a released lease must let writers open again");
}

/// A writer racing a full publication is never silently dropped: either it lands
/// in the published generation, or the publication fails closed and it survives
/// in the destination that was preserved.
#[test]
fn a_concurrent_writer_is_never_silently_dropped_by_publication() {
    for attempt in 0..6 {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("work.sqlite");
        drop(Store::open(&target).unwrap());

        let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();
        let raced = control_record("raced write");
        let writer_record = raced.clone();
        let writer_target = target.clone();
        let writer = std::thread::spawn(move || {
            let store = Store::open(&writer_target).unwrap();
            store.upsert_record(&writer_record).unwrap();
        });
        let published = builder.finish();
        writer.join().unwrap();

        let store = Store::open_read_only(&target).unwrap();
        let survived = store.get_record(raced.id).is_ok();
        drop(store);
        assert!(
            survived,
            "attempt {attempt}: the raced write was dropped (publication {})",
            if published.is_ok() {
                "succeeded"
            } else {
                "failed"
            }
        );
    }
}

#[test]
fn cold_search_validation_is_read_only_and_detects_projection_count_drift() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("stage.sqlite")).unwrap();
    store
        .upsert_record(&HistoryRecord {
            id: Uuid::new_v4(),
            title: "cold validation".to_owned(),
            body: "検索投影の完全な入力".to_owned(),
            tags: vec!["cold".to_owned()],
            kind: "task".to_owned(),
            workspace: Some("/workspace".to_owned()),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            updated_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
        })
        .unwrap();
    let expected = store.rebuild_search_projection_with_counts().unwrap();
    assert_eq!(expected.history_search, 1);
    assert_eq!(expected.history_scriptgram, 1);

    store
        .conn
        .authorizer(Some(|context: AuthContext<'_>| match context.action {
            AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. } => {
                Authorization::Deny
            }
            _ => Authorization::Allow,
        }));
    let started = Instant::now();
    validate_search_projection(&store, expected).unwrap();
    let elapsed = started.elapsed();
    store
        .conn
        .authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    eprintln!("bounded cold search validation: {elapsed:?}");

    store
        .conn
        .execute("DELETE FROM ctx_history_search", [])
        .unwrap();
    assert!(matches!(
        validate_search_projection(&store, expected),
        Err(StoreError::ColdStoreValidation(message))
            if message == "rebuilt search authority does not match canonical rows"
    ));
}
