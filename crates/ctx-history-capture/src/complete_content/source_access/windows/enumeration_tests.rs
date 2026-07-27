use super::*;
use std::time::Instant;

#[test]
fn retained_directory_enumeration_stops_at_the_entry_bound() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..64 {
        fs::write(temp.path().join(format!("{index:03}.json")), b"{}").unwrap();
    }
    let admitted = admit_path(temp.path(), None, Uuid::new_v4()).unwrap();
    let AdmittedWindowsPath::Directory(directory) = admitted else {
        panic!("root must be admitted as a directory");
    };
    let error = match directory_entries(
        &directory,
        8,
        Instant::now() + std::time::Duration::from_secs(1),
        Uuid::new_v4(),
    ) {
        Ok(_) => panic!("oversized directory enumeration must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind, CompleteContentErrorKind::ContentTooLarge);
}

#[test]
fn retained_directory_enumeration_stops_at_the_deadline() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("entry.json"), b"{}").unwrap();
    let admitted = admit_path(temp.path(), None, Uuid::new_v4()).unwrap();
    let AdmittedWindowsPath::Directory(directory) = admitted else {
        panic!("root must be admitted as a directory");
    };
    let error = match directory_entries(&directory, 8, Instant::now(), Uuid::new_v4()) {
        Ok(_) => panic!("expired directory enumeration must fail"),
        Err(error) => error,
    };
    assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
}

#[test]
fn file_identity_debug_omits_the_final_path() {
    let identity = WindowsFileIdentity {
        volume_serial_number: 7,
        file_id: [3; 16],
        change_time: 11,
        last_write_time: 13,
        attributes: 0,
        length: 17,
        final_path: PathBuf::from(r"C:\secret\provider\session.jsonl"),
    };
    let debug = format!("{identity:?}");
    assert!(debug.contains("WindowsFileIdentity"));
    assert!(!debug.contains("secret"));
    assert!(!debug.contains("final_path"));
}
