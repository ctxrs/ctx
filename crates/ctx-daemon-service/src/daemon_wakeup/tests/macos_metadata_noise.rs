use super::*;
use std::{io::Write, time::Instant};

#[test]
fn sibling_file_changes_do_not_invalidate_provider_routes_or_config() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let home = temp.path().join("home");
    let data_root = home.join(".ctx");
    let config = data_root.join(CONFIG_FILE);
    let codex_root = home.join(".codex/sessions");
    let claude_root = home.join(".claude/projects");
    let source = codex_root.join("session.jsonl");
    let missing_source = home.join(".opencode/history.db");
    fs::create_dir_all(data_root.join("daemon")).unwrap();
    fs::create_dir_all(&codex_root).unwrap();
    fs::create_dir_all(&claude_root).unwrap();
    fs::write(&config, b"[indexing]\nmode = \"auto\"\n").unwrap();
    fs::write(&source, b"{\"event\":1}\n").unwrap();
    let catalog = watch_catalog([
        catalog_route(
            CaptureProvider::Codex,
            codex_root,
            "codex_session_jsonl_tree",
        ),
        catalog_route(
            CaptureProvider::Claude,
            claude_root,
            "claude_projects_jsonl_tree",
        ),
        catalog_route(CaptureProvider::OpenCode, missing_source, "opencode_sqlite"),
    ]);
    let source_route = catalog
        .routes_overlapping_path(&source)
        .into_iter()
        .next()
        .unwrap();
    let roots = ctx_daemon_runtime::watch_roots(catalog.target_paths());
    assert_eq!(roots.get(&home), Some(&false));
    assert_eq!(catalog.routes_overlapping_path(&home).len(), 3);

    let wakeup = Arc::new(DaemonWakeup::default());
    let watcher =
        DaemonFileWatcher::start(&data_root, Arc::clone(&wakeup), catalog_owner(catalog)).unwrap();
    let settled = Instant::now() + Duration::from_secs(2);
    while let Some(remaining) = settled.checked_duration_since(Instant::now()) {
        if wakeup.wait(remaining).timed_out {
            break;
        }
    }

    let original_config = fs::read(&config).unwrap();
    let config_modified = fs::metadata(&config).unwrap().modified().unwrap();
    let home_modified = fs::metadata(&home).unwrap().modified().unwrap();
    let sibling_lock = home.join(".claude.json.lock");
    let sibling_temp = home.join(".claude.json.tmp");
    fs::create_dir(&sibling_lock).unwrap();
    fs::write(&sibling_temp, b"{}\n").unwrap();
    fs::rename(&sibling_temp, home.join(".claude.json")).unwrap();
    fs::remove_dir(&sibling_lock).unwrap();
    assert_ne!(
        fs::metadata(&home).unwrap().modified().unwrap(),
        home_modified
    );

    // Cross the five-second metadata fallback interval as well as native debounce.
    let idle = wakeup.wait(Duration::from_secs(8));
    assert!(
        idle.timed_out && !idle.filesystem && idle.source_watch.is_empty(),
        "unrelated HOME metadata must not schedule provider or config work: {idle:?}"
    );
    assert!(wakeup.pending_source_watch().is_empty());
    assert_eq!(fs::read(&config).unwrap(), original_config);
    assert_eq!(
        fs::metadata(&config).unwrap().modified().unwrap(),
        config_modified
    );

    let mut writer = fs::OpenOptions::new().append(true).open(&source).unwrap();
    writer.write_all(b"{\"event\":2}\n").unwrap();
    writer.flush().unwrap();
    let appended = wakeup.wait(Duration::from_secs(8));
    assert!(appended.filesystem, "source append must wake the daemon");
    assert_eq!(appended.source_watch.routes.len(), 1);
    assert!(appended.source_watch.routes.contains_key(&source_route));
    assert!(appended.source_watch.reconcile.is_none());
    drop(writer);

    fs::write(&config, b"[indexing]\nmode = \"manual\"\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut config_changed = false;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let wake = wakeup.wait(remaining);
        if wake.source_watch.reconcile.is_some() {
            config_changed = true;
            break;
        }
        if wake.timed_out {
            break;
        }
    }
    assert!(
        config_changed,
        "a real config edit must still reconcile the catalog"
    );
    drop(watcher);
}
