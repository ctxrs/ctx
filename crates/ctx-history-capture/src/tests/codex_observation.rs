use super::support::*;

#[cfg(unix)]
#[test]
fn codex_catalog_cache_reparses_same_size_rewrite_with_restored_mtime() {
    let temp = tempdir();
    let root = temp.path().join("sessions");
    let path = root.join("2026/07/16/rewrite.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let first_line = serde_json::json!({
        "timestamp": "2026-07-16T12:00:00Z",
        "type": "session_meta",
        "payload": {"id": "same-stat-codex", "cwd": "/repo-a"}
    });
    let second_line = serde_json::json!({
        "timestamp": "2026-07-16T12:00:00Z",
        "type": "session_meta",
        "payload": {"id": "same-stat-codex", "cwd": "/repo-b"}
    });
    let first_body = format!("{first_line}\n");
    let second_body = format!("{second_line}\n");
    assert_eq!(first_body.len(), second_body.len());
    fs::write(&path, first_body).unwrap();
    let original_modified = fs::metadata(&path).unwrap().modified().unwrap();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    catalog_codex_session_tree(&root, &store, CodexSessionCatalogOptions::default()).unwrap();
    let first = store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, root.to_str().unwrap())
        .unwrap()
        .pop()
        .unwrap();

    fs::write(&path, second_body).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    let summary =
        catalog_codex_session_tree(&root, &store, CodexSessionCatalogOptions::default()).unwrap();
    let second = store
        .list_catalog_sessions_for_source(CaptureProvider::Codex, root.to_str().unwrap())
        .unwrap()
        .pop()
        .unwrap();

    assert_eq!(summary.cached_sessions, 0);
    assert_eq!(summary.parsed_sessions, 1);
    assert_eq!(second.cwd.as_deref(), Some("/repo-b"));
    assert_ne!(
        first.metadata.get("file_observation_token_v1"),
        second.metadata.get("file_observation_token_v1")
    );
}
