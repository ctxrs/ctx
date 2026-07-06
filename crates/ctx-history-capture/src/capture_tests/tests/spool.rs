#[allow(unused_imports)]
use super::*;

#[cfg(unix)]
#[test]
pub(crate) fn import_rejects_symlink_pending_spool_entry() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    let target = temp.path().join("outside.jsonl");
    fs::write(&target, "not json\n").unwrap();
    let pending = inbox.join("capture-link.jsonl");
    symlink(&target, &pending).unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    assert!(matches!(
        import_spool(&inbox, &mut store),
        Err(CaptureError::InvalidPath(path)) if path.ends_with("capture-link.jsonl")
    ));
    assert!(pending.exists());
    assert_eq!(fs::read_to_string(target).unwrap(), "not json\n");
}
