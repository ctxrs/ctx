use std::fs;

use ctx_history_core::CaptureProvider;

use super::*;

fn discovered_leaf() -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    DirectJsonlInventoryLeaf,
) {
    let temp = tempfile::tempdir().unwrap();
    let ancestor = temp.path().join("authority");
    let root = ancestor.join("transcripts");
    let leaf = root.join("session.jsonl");
    fs::create_dir_all(&root).unwrap();
    fs::write(&leaf, b"{\"type\":\"message\"}\n").unwrap();
    let adapter = DirectJsonlSourceAdapter::new(
        CaptureProvider::Windsurf,
        "windsurf_hook_transcript_jsonl",
        "windsurf-hook-jsonl-v1",
    );
    let inventory = adapter.discover(&root).unwrap();
    assert_eq!(inventory.leaves.len(), 1);
    let retained = inventory.leaves[0].clone();
    (temp, ancestor, root, retained)
}

#[test]
fn shared_native_jsonl_rejects_root_swap_after_discovery() {
    let (_temp, _ancestor, root, retained) = discovered_leaf();
    let displaced = root.with_file_name("transcripts-displaced");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("session.jsonl"), b"{\"replacement\":true}\n").unwrap();

    assert!(retained.open_verified().is_err());
}

#[test]
fn shared_native_jsonl_rejects_ancestor_swap_after_discovery() {
    let (temp, ancestor, root, retained) = discovered_leaf();
    let displaced = temp.path().join("authority-displaced");
    fs::rename(&ancestor, &displaced).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("session.jsonl"), b"{\"replacement\":true}\n").unwrap();

    assert!(retained.open_verified().is_err());
}

#[test]
fn shared_native_jsonl_rejects_leaf_swap_after_discovery() {
    let (_temp, _ancestor, root, retained) = discovered_leaf();
    let leaf = root.join("session.jsonl");
    fs::rename(&leaf, root.join("session-displaced.jsonl")).unwrap();
    fs::write(&leaf, b"{\"replacement\":true}\n").unwrap();

    assert!(retained.open_verified().is_err());
}
