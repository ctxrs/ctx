use crate::tests::support::paths::{provider_history_fixture, tempdir};
use crate::tests::support::provider_state::{
    provider_import_session_id_for_path, stored_provider_session_id,
};
#[cfg(unix)]
use crate::{import_codex_session_jsonl, CaptureError, ProviderImportSummary};
use crate::{import_codex_session_paths, import_codex_session_tree, CodexSessionImportOptions};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use std::fs;
use std::sync::Arc;

#[cfg(unix)]
fn assert_catalog_path_rejected(summary: &ProviderImportSummary, suffix: &str) {
    assert_eq!(summary.failed, 1, "{:?}", summary.failures);
    assert_eq!(summary.failures.len(), 1, "{:?}", summary.failures);
    assert_eq!(summary.failures[0].line, 0);
    assert!(
        summary.failures[0].error.contains(suffix)
            && summary.failures[0]
                .error
                .contains("invalid provider transcript path"),
        "{:?}",
        summary.failures
    );
}

#[test]
fn codex_session_tree_defers_cross_file_child_edges_until_parent_is_known() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-out-of-order-sessions");
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_tree(
        &fixture,
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T02:15:00Z".parse().unwrap(),
            max_session_files: Some(usize::MAX),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 2);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(summary.imported_edges, 1);

    let parent_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-out-of-order-root");
    let child_id =
        stored_provider_session_id(&store, CaptureProvider::Codex, "codex-out-of-order-child");
    let child = store.get_session(child_id).unwrap();
    assert_eq!(child.parent_session_id, Some(parent_id));
    assert_eq!(child.root_session_id, Some(parent_id));
}

#[test]
fn codex_session_paths_imports_only_explicit_subset() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    let total_bytes = fs::metadata(&fixture).unwrap().len();
    let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = Arc::clone(&progress);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_codex_session_paths(
        vec![fixture.clone()],
        &mut store,
        CodexSessionImportOptions {
            source_path: Some(fixture.clone()),
            imported_at: "2026-06-24T02:30:00Z".parse().unwrap(),
            progress: Some(Arc::new(move |progress| {
                observed.lock().unwrap().push((
                    progress.total_files,
                    progress.total_bytes,
                    progress.done,
                ));
            })),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 5);
    assert_eq!(summary.skipped_events, 1);
    assert_eq!(summary.imported_edges, 0);
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    let root_id = stored_provider_session_id(&store, CaptureProvider::Codex, "codex-session-root");
    let child_id = provider_import_session_id_for_path(
        CaptureProvider::Codex,
        "codex_session_jsonl",
        &fixture,
        "codex-session-child",
    );
    assert_eq!(store.events_for_session(root_id).unwrap().len(), 5);
    assert!(store.events_for_session(child_id).unwrap().is_empty());

    let progress = progress.lock().unwrap();
    assert!(progress
        .iter()
        .all(|(files, bytes, _)| { *files == 1 && *bytes == total_bytes }));
    assert_eq!(progress.last().map(|(_, _, done)| *done), Some(true));
}

#[test]
fn codex_session_paths_reimport_skips_existing_events() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23");
    let paths = vec![fixture.join("root.jsonl"), fixture.join("subagent.jsonl")];
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let first = import_codex_session_paths(
        paths.clone(),
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-24T02:45:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(first.failed, 0, "{:?}", first.failures);
    assert_eq!(first.imported_sessions, 2);
    assert_eq!(first.imported_events, 7);
    assert_eq!(first.skipped_events, 1);
    assert_eq!(first.imported_edges, 1);

    let second = import_codex_session_paths(
        paths,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-24T02:45:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(second.failed, 0, "{:?}", second.failures);
    assert_eq!(second.imported_sessions, 0);
    assert_eq!(second.imported_events, 0);
    assert_eq!(second.imported_edges, 0);
    assert_eq!(second.skipped_sessions, 2);
    assert_eq!(second.skipped_events, 8);
    assert_eq!(second.skipped_edges, 0);
}

#[cfg(unix)]
#[test]
fn codex_session_paths_rejects_symlinked_jsonl_files() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    let link = temp.path().join("linked-root.jsonl");
    symlink(&fixture, &link).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_paths(
        vec![link],
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-24T03:00:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_catalog_path_rejected(&summary, "linked-root.jsonl");
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn codex_session_file_rejects_symlinked_jsonl_files() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    let link = temp.path().join("linked-root.jsonl");
    symlink(&fixture, &link).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &link,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_catalog_path_rejected(&summary, "linked-root.jsonl");
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn codex_session_file_rejects_symlinked_parent_components() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let real_dir = temp.path().join("real-parent");
    fs::create_dir_all(&real_dir).unwrap();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23/root.jsonl");
    fs::copy(&fixture, real_dir.join("root.jsonl")).unwrap();
    let link_dir = temp.path().join("linked-parent");
    symlink(&real_dir, &link_dir).unwrap();
    let linked_file = link_dir.join("root.jsonl");

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let summary = import_codex_session_jsonl(
        &linked_file,
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();

    assert_catalog_path_rejected(&summary, "linked-parent/root.jsonl");
    assert!(store.list_sessions().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn codex_session_tree_rejects_symlinked_jsonl_files() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions").join("2026/06/23");
    let sessions = temp.path().join("sessions/2026/06/23");
    fs::create_dir_all(&sessions).unwrap();
    symlink(fixture.join("root.jsonl"), sessions.join("root.jsonl")).unwrap();

    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let err = import_codex_session_tree(
        temp.path().join("sessions"),
        &mut store,
        CodexSessionImportOptions {
            imported_at: "2026-06-23T16:30:00Z".parse().unwrap(),
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap_err();

    assert!(matches!(
        err,
        CaptureError::InvalidProviderTranscriptPath { path, reason }
            if path.ends_with("root.jsonl")
                && reason == "linked provider transcript path components are rejected"
    ));
    assert!(store.list_sessions().unwrap().is_empty());
}
