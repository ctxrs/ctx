use super::*;
#[test]
fn optional_counters_skip_contention_and_accept_the_released_lock() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state.json");
    let mut first = StateFile::try_open(&path).unwrap().unwrap();
    first.write(&[4u64, 7]).unwrap();
    assert!(StateFile::try_open(&path).unwrap().is_none());
    assert_eq!(fs::read(&path).unwrap(), b"[4,7]");
    drop(first);
    let mut next = StateFile::try_open(&path).unwrap().unwrap();
    assert_eq!(next.read::<Vec<u64>>().unwrap(), Some(vec![4, 7]));
}
#[cfg(unix)]
#[test]
fn optional_counters_do_not_follow_symlinks_or_modify_their_targets() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("ordinary.txt");
    let path = temp.path().join("state.json");
    fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(StateFile::try_open(&path).is_err());
    assert_eq!(fs::read(target).unwrap(), b"keep");
}
