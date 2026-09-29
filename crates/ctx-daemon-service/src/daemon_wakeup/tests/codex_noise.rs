use super::*;

#[cfg(unix)]
#[test]
fn codex_non_source_events_do_not_advance_route_watermarks_or_members() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let daemon_root = data_root.join("daemon");
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let noise = root.join("scratch.txt");
    let rollout = root.join("session.jsonl");
    let compressed = root.join("session.jsonl.zst");
    let alias = root.join("alias.txt");
    for path in [&noise, &rollout, &compressed] {
        fs::write(path, b"fixture\n").unwrap();
    }
    fs::hard_link(&rollout, &alias).unwrap();
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        root,
        "codex_session_jsonl_tree",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let authority = RwLock::new(watch_authority(&data_root, catalog));
    let counters = Mutex::new(WatchCounters::default());
    let wakeup = DaemonWakeup::default();
    let observed = Arc::new(Mutex::new(SourceWatchBatch::default()));
    let sink = Arc::clone(&observed);
    wakeup.install_source_watch_sink(Arc::new(move |batch| {
        sink.lock().unwrap().merge(batch.clone());
    }));

    for (sequence, paths, expected) in [
        (1, vec![noise.clone()], None),
        (2, vec![rollout.clone()], Some(rollout.clone())),
        (3, vec![noise.clone()], None),
        (4, vec![noise, compressed.clone()], Some(compressed.clone())),
        (5, vec![alias.clone()], Some(alias.clone())),
    ] {
        let watermark = EventWatermark::new(17, sequence);
        let batch = record_and_observe_watch_event(
            &authority,
            &counters,
            &wakeup,
            &data_root,
            &daemon_root,
            Ok(NativeWatchEvent::ordinary(paths)),
            watermark,
        );
        if let Some(member) = expected {
            assert_eq!(batch.routes.get(&route), Some(&watermark));
            assert_eq!(batch.members.get(&route), Some(&BTreeSet::from([member])));
        } else {
            assert!(batch.is_empty());
            assert_eq!(
                observed.lock().unwrap().routes.get(&route).copied(),
                (sequence == 3).then_some(EventWatermark::new(17, 2)),
                "noise must not add or advance the publication handoff fence"
            );
        }
        assert!(batch.reconcile.is_none());
        assert!(!batch.rearm);
    }
    let observed = observed.lock().unwrap();
    assert_eq!(
        observed.routes.get(&route),
        Some(&EventWatermark::new(17, 5))
    );
    assert_eq!(
        observed.members.get(&route),
        Some(&BTreeSet::from([rollout, compressed, alias]))
    );
    assert_eq!(counters.lock().unwrap().ignored_other_events, 2);
}

#[test]
fn codex_non_source_filter_retains_rename_delete_directory_and_uncertainty() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let daemon_root = data_root.join("daemon");
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let rollout = root.join("session.jsonl");
    let renamed = root.join("renamed.txt");
    fs::write(&rollout, b"fixture\n").unwrap();
    let catalog = watch_catalog([catalog_route(
        CaptureProvider::Codex,
        root.clone(),
        "codex_session_jsonl_tree",
    )]);
    let route = catalog.route_ids().next().unwrap().clone();
    let authority = RwLock::new(watch_authority(&data_root, catalog));
    let counters = Mutex::new(WatchCounters::default());
    let classify = |event, sequence| {
        record_watch_event(
            &authority,
            &counters,
            &data_root,
            &daemon_root,
            Ok(event),
            EventWatermark::new(18, sequence),
        )
    };

    fs::rename(&rollout, &renamed).unwrap();
    let rename = classify(
        NativeWatchEvent::requiring_rearm(vec![rollout, renamed.clone()]),
        1,
    );
    assert_eq!(rename.routes.get(&route), Some(&EventWatermark::new(18, 1)));
    assert!(rename.members.is_empty());
    assert!(rename.rearm);

    // Unknown create/modify and rename notifications retain their rearm path
    // even when the reported target now exists as a regular non-source file.
    let ambiguous = classify(NativeWatchEvent::requiring_rearm(vec![renamed.clone()]), 2);
    assert_eq!(
        ambiguous.routes.get(&route),
        Some(&EventWatermark::new(18, 2))
    );
    assert!(ambiguous.rearm);
    let rescan = classify(NativeWatchEvent::rescan(vec![renamed.clone()]), 3);
    assert_eq!(rescan.reconcile, Some(EventWatermark::new(18, 3)));
    assert!(rescan.rearm);

    fs::remove_file(&renamed).unwrap();
    let deleted = classify(NativeWatchEvent::ordinary(vec![renamed]), 4);
    assert_eq!(
        deleted.routes.get(&route),
        Some(&EventWatermark::new(18, 4))
    );
    assert!(deleted.members.is_empty());
    let directory = root.join("directory.txt");
    fs::create_dir(&directory).unwrap();
    let created = classify(
        NativeWatchEvent::requiring_rearm(vec![directory.clone()]),
        5,
    );
    assert_eq!(
        created.routes.get(&route),
        Some(&EventWatermark::new(18, 5))
    );
    assert!(created.members.is_empty());
    assert!(created.rearm);
    let changed = classify(NativeWatchEvent::ordinary(vec![directory]), 6);
    assert_eq!(
        changed.routes.get(&route),
        Some(&EventWatermark::new(18, 6))
    );
    assert!(changed.members.is_empty());
}

#[cfg(unix)]
#[test]
fn codex_filter_preserves_overlapping_database_and_control_events() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let daemon_root = data_root.join("daemon");
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let database = root.join("history.db");
    let control = root.join("control.txt");
    fs::write(&database, b"fixture\n").unwrap();
    fs::write(&control, b"fixture\n").unwrap();
    let catalog = watch_catalog([
        catalog_route(CaptureProvider::Codex, root, "codex_session_jsonl_tree"),
        catalog_route(
            CaptureProvider::OpenCode,
            database.clone(),
            "opencode_sqlite",
        ),
    ]);
    let database_route = catalog
        .route_ids_for_provider(CaptureProvider::OpenCode)
        .pop_first()
        .unwrap();
    let mut authority = watch_authority(&data_root, catalog);
    authority.controls.insert(control.clone());
    let authority = RwLock::new(authority);
    let counters = Mutex::new(WatchCounters::default());
    let database_batch = record_watch_event(
        &authority,
        &counters,
        &data_root,
        &daemon_root,
        Ok(NativeWatchEvent::ordinary(vec![database])),
        EventWatermark::new(19, 1),
    );
    assert_eq!(
        database_batch.routes,
        BTreeMap::from([(database_route, EventWatermark::new(19, 1))])
    );
    assert!(database_batch.members.is_empty());
    let control_batch = record_watch_event(
        &authority,
        &counters,
        &data_root,
        &daemon_root,
        Ok(NativeWatchEvent::ordinary(vec![control])),
        EventWatermark::new(19, 2),
    );
    assert_eq!(control_batch.reconcile, Some(EventWatermark::new(19, 2)));
}

#[cfg(target_os = "linux")]
#[test]
fn native_codex_noise_is_ignored_but_rollout_and_alias_appends_wake() {
    use std::{fs::OpenOptions, io::Write, time::Instant};

    for through_alias in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let root = temp.path().join("sessions");
        fs::create_dir(&data_root).unwrap();
        fs::create_dir(&root).unwrap();
        let noise = root.join("scratch.txt");
        let rollout = root.join("session.jsonl");
        fs::write(&noise, b"scratch\n").unwrap();
        fs::write(&rollout, b"{\"event\":1}\n").unwrap();
        let target = if through_alias {
            let alias = rollout.with_file_name("alias.txt");
            fs::hard_link(&rollout, &alias).unwrap();
            alias
        } else {
            rollout.clone()
        };
        let catalog = watch_catalog([catalog_route(
            CaptureProvider::Codex,
            root,
            "codex_session_jsonl_tree",
        )]);
        let route = catalog.route_ids().next().unwrap().clone();
        let wakeup = Arc::new(DaemonWakeup::default());
        let watcher =
            DaemonFileWatcher::start(&data_root, Arc::clone(&wakeup), catalog_owner(catalog))
                .unwrap();

        OpenOptions::new()
            .append(true)
            .open(&noise)
            .unwrap()
            .write_all(b"more scratch\n")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while watcher.lock_counters().ignored_other_events == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            watcher.lock_counters().ignored_other_events > 0,
            "native noise event was not observed"
        );
        let idle = wakeup.wait(WATCH_DEBOUNCE_QUIET * 2);
        assert!(
            !idle.filesystem,
            "non-source append must not wake a refresh"
        );
        assert!(idle.source_watch.is_empty());
        assert!(wakeup.pending_source_watch().is_empty());

        OpenOptions::new()
            .append(true)
            .open(&target)
            .unwrap()
            .write_all(b"{\"event\":2}\n")
            .unwrap();
        let wake = wakeup.wait(Duration::from_secs(3));
        assert!(
            wake.filesystem,
            "rollout append through {target:?} must still wake a refresh"
        );
        assert_eq!(wake.source_watch.routes.len(), 1);
        assert_eq!(
            wake.source_watch.members.get(&route),
            Some(&BTreeSet::from([target]))
        );
        assert!(wake.source_watch.reconcile.is_none());
        assert_eq!(
            fs::read(&rollout).unwrap(),
            b"{\"event\":1}\n{\"event\":2}\n"
        );
        drop(watcher);
    }
}
