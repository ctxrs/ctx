#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn failed_import_retains_raw_failed_file_and_error_metadata() {
    let temp = tempdir();
    let inbox = temp.path().join("inbox");
    fs::create_dir_all(&inbox).unwrap();
    let pending = inbox.join("capture-bad.jsonl");
    fs::write(&pending, "not json\n").unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_spool(&inbox, &mut store).unwrap();

    assert_eq!(summary.failed_files, 1);
    assert_eq!(summary.processed_files, 1);
    let failed = inbox.join("capture-bad.jsonl.failed");
    let sidecar = inbox.join("capture-bad.jsonl.failed.error.json");
    assert!(failed.exists());
    assert!(sidecar.exists());
    assert_eq!(fs::read_to_string(failed).unwrap(), "not json\n");
    assert!(fs::read_to_string(sidecar)
        .unwrap()
        .contains("not a valid capture envelope"));
    assert_eq!(spool_counts(&inbox).unwrap().failed, 1);
}
