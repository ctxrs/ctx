use super::*;

#[test]
fn codex_tree_filter_keeps_rollouts_and_uncertain_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir_all(root.join("nested")).unwrap();
    let (catalog, route) = catalog_for(automatic_route(
        CaptureProvider::Codex,
        root.clone(),
        "codex_session_jsonl_tree",
    ));
    for (name, ignored) in [
        (".scratch.txt", cfg!(unix)),
        ("session.jsonl.backup", cfg!(unix)),
        ("renamed-session.jsonl", false),
        ("renamed-session.jsonl.zst", false),
    ] {
        let file = root.join("nested").join(name);
        fs::write(&file, b"fixture\n").unwrap();
        assert_eq!(
            catalog.event_is_known_non_source_file(&route, &file),
            ignored,
            "{name}"
        );
    }
    let directory = root.join("directory.txt");
    fs::create_dir(&directory).unwrap();
    for path in [&root, &directory, &root.join("missing.txt")] {
        assert!(!catalog.event_is_known_non_source_file(&route, path));
    }
    let removed = root.join("removed.txt");
    fs::write(&removed, b"fixture\n").unwrap();
    fs::remove_file(&removed).unwrap();
    assert!(!catalog.event_is_known_non_source_file(&route, &removed));
}

#[cfg(any(unix, windows))]
#[test]
fn codex_non_source_filter_retains_hard_link_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let rollout = root.join("session.jsonl");
    let alias = root.join("alias.txt");
    let ordinary = root.join("scratch.txt");
    fs::write(&rollout, b"fixture\n").unwrap();
    fs::write(&ordinary, b"scratch\n").unwrap();
    fs::hard_link(&rollout, &alias).unwrap();
    let (catalog, route) = catalog_for(automatic_route(
        CaptureProvider::Codex,
        root,
        "codex_session_jsonl_tree",
    ));

    assert!(!catalog.event_is_known_non_source_file(&route, &alias));
    assert!(!catalog.event_is_known_non_source_file(&route, &rollout));
    // Only Unix supplies the metadata-only single-link proof used by the filter.
    assert_eq!(
        catalog.event_is_known_non_source_file(&route, &ordinary),
        cfg!(unix)
    );
}

#[test]
fn non_source_filter_requires_one_known_codex_directory_registration() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let nested = root.join("nested");
    let file = nested.join("scratch.txt");
    fs::create_dir_all(&nested).unwrap();
    fs::write(&file, b"fixture\n").unwrap();
    for (provider, path, format) in [
        (
            CaptureProvider::Codex,
            file.clone(),
            "codex_session_jsonl_tree",
        ),
        (CaptureProvider::Codex, root.clone(), "codex_history_jsonl"),
        (
            CaptureProvider::Claude,
            root.clone(),
            "claude_projects_jsonl_tree",
        ),
        (CaptureProvider::OpenCode, file.clone(), "opencode_sqlite"),
    ] {
        let (catalog, route) = catalog_for(automatic_route(provider, path, format));
        assert!(!catalog.event_is_known_non_source_file(&route, &file));
    }
    let mut route = automatic_route(CaptureProvider::Codex, root, "codex_session_jsonl_tree");
    route.registration_sources.push(source(
        CaptureProvider::Codex,
        nested,
        "codex_session_jsonl_tree",
    ));
    let (catalog, route) = catalog_for(route);
    assert!(!catalog.event_is_known_non_source_file(&route, &file));
    assert!(!SourceBackedWatchCatalog::default().event_is_known_non_source_file(&route, &file));
}

#[cfg(unix)]
#[test]
fn codex_non_source_filter_retains_symlink_files_and_ancestors() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let outside = temp.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("scratch.txt"), b"fixture\n").unwrap();
    symlink(outside.join("scratch.txt"), root.join("link.txt")).unwrap();
    symlink(&outside, root.join("linked-directory")).unwrap();
    let (catalog, route) = catalog_for(automatic_route(
        CaptureProvider::Codex,
        root.clone(),
        "codex_session_jsonl_tree",
    ));
    for path in [
        root.join("link.txt"),
        root.join("linked-directory/scratch.txt"),
    ] {
        assert!(!catalog.event_is_known_non_source_file(&route, &path));
    }
}
