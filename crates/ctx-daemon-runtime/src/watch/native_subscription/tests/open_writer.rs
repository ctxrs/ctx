use std::{io::Write, time::Instant};

use super::{mpsc, Arc, BTreeMap, Duration, NativeFileWatcher, TestPayload};

#[test]
fn already_dirty_open_writer_append_wakes_refresh() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let source = temp.path().join("active.jsonl");
    let mut writer = std::fs::OpenOptions::new()
        .create_new(true)
        .append(true)
        .open(&source)
        .unwrap();
    writer.write_all(b"dirty before registration\n").unwrap();
    writer.flush().unwrap();
    let (tx, rx) = mpsc::channel();
    let mut watcher = NativeFileWatcher::start(
        "ctx-open-writer-test",
        Arc::new(|_| false),
        Arc::new(move |event, watermark| {
            if let Ok(event) = event {
                let _ = tx.send(event.paths);
            }
            TestPayload(Some(watermark))
        }),
        Arc::new(|_| {}),
        Arc::new(|watermark| TestPayload(Some(watermark))),
        Arc::new(|_| {}),
        Arc::new(|_| {}),
    )
    .unwrap();
    watcher
        .reconcile_paths(BTreeMap::from([(temp.path().to_path_buf(), true)]), false)
        .unwrap();

    // Discard startup activity so an earlier notification cannot satisfy the append.
    let settled = Instant::now() + Duration::from_secs(2);
    while let Some(remaining) = settled.checked_duration_since(Instant::now()) {
        if rx.recv_timeout(remaining).is_err() {
            break;
        }
    }
    writer.write_all(b"appended while still open\n").unwrap();
    writer.flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut observed = false;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(paths) if paths.contains(&source) => {
                observed = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(observed, "refresh must wake before the open writer closes");
    drop(writer);
}
