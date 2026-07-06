#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn source_import_manifest_upsert_ignores_observed_at_for_unchanged_files() {
    let temp = tempdir();
    let store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let observed_at_ms = timestamp_ms(fixed_time());
    let mut file = SourceImportFile {
        provider: CaptureProvider::Claude,
        source_format: "claude_projects_jsonl_tree".into(),
        source_root: "/home/user/.claude/projects".into(),
        source_path: "/home/user/.claude/projects/session.jsonl".into(),
        file_size_bytes: 42,
        file_modified_at_ms: observed_at_ms,
        observed_at_ms,
        metadata: serde_json::json!({}),
    };
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    store
        .mark_source_import_file_indexed(
            CaptureProvider::Claude,
            SourceImportFileIndexUpdate {
                source_root: "/home/user/.claude/projects",
                source_path: "/home/user/.claude/projects/session.jsonl",
                file_size_bytes: 42,
                file_modified_at_ms: observed_at_ms,
                indexed_at_ms: observed_at_ms + 10,
            },
        )
        .unwrap();
    let after_indexed: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();

    file.observed_at_ms += 1_000;
    store
        .upsert_source_import_files(std::slice::from_ref(&file))
        .unwrap();
    let after_noop: i64 = store
        .conn
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(after_noop, after_indexed);
    assert!(store
        .list_pending_source_import_files(CaptureProvider::Claude, "/home/user/.claude/projects")
        .unwrap()
        .is_empty());
}
