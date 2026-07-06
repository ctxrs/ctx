#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn import_rejects_non_regular_pending_spool_entry() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    fs::create_dir_all(inbox.join("capture-dir.jsonl")).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        import_spool(&inbox, &mut store),
        Err(CaptureError::InvalidPath(path)) if path.ends_with("capture-dir.jsonl")
    ));
    assert!(inbox.join("capture-dir.jsonl").is_dir());
}
