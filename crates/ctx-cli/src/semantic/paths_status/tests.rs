use super::*;
use std::sync::{Arc, Barrier};

#[test]
fn durable_state_path_is_purpose_based() {
    assert_eq!(
        daemon_core_refresh_job_path(Path::new("ctx-data")),
        Path::new("ctx-data/daemon/jobs/core-refresh.json")
    );
}

#[test]
fn concurrent_status_writers_use_distinct_atomic_staging_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = Arc::new(temp.path().join("daemon/jobs/core-refresh.json"));
    let barrier = Arc::new(Barrier::new(16));
    let writers = (0..16)
        .map(|writer| {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for iteration in 0..32 {
                    write_private_json_file(
                        path.as_ref(),
                        &json!({"writer": writer, "iteration": iteration}),
                    )
                    .unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }

    let published: Value = serde_json::from_slice(&fs::read(path.as_ref()).unwrap()).unwrap();
    assert!(published["writer"].as_u64().is_some());
    assert!(published["iteration"].as_u64().is_some());
    assert!(fs::read_dir(path.parent().unwrap())
        .unwrap()
        .all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
}

#[test]
fn windows_private_file_replacement_retries_transient_lock_errors() {
    let errors = [
        WINDOWS_ERROR_ACCESS_DENIED,
        WINDOWS_ERROR_SHARING_VIOLATION,
        WINDOWS_ERROR_LOCK_VIOLATION,
    ];
    let mut attempts = 0;
    let mut waits = 0;
    retry_windows_private_file_replacement(
        || {
            let attempt = attempts;
            attempts += 1;
            if let Some(code) = errors.get(attempt) {
                Err(std::io::Error::from_raw_os_error(*code))
            } else {
                Ok(())
            }
        },
        || waits += 1,
    )
    .unwrap();

    assert_eq!(attempts, 4);
    assert_eq!(waits, 3);
}

#[test]
fn windows_private_file_replacement_does_not_retry_other_errors() {
    let mut attempts = 0;
    let mut waits = 0;
    let error = retry_windows_private_file_replacement(
        || {
            attempts += 1;
            Err(std::io::Error::from_raw_os_error(2))
        },
        || waits += 1,
    )
    .unwrap_err();

    assert_eq!(error.raw_os_error(), Some(2));
    assert_eq!(attempts, 1);
    assert_eq!(waits, 0);
}

#[test]
fn windows_private_file_replacement_has_a_bounded_retry_window() {
    let mut attempts = 0;
    let mut waits = 0;
    let error = retry_windows_private_file_replacement(
        || {
            attempts += 1;
            Err(std::io::Error::from_raw_os_error(
                WINDOWS_ERROR_ACCESS_DENIED,
            ))
        },
        || waits += 1,
    )
    .unwrap_err();

    assert_eq!(error.raw_os_error(), Some(WINDOWS_ERROR_ACCESS_DENIED));
    assert_eq!(attempts, PRIVATE_FILE_REPLACE_ATTEMPTS);
    assert_eq!(waits, PRIVATE_FILE_REPLACE_ATTEMPTS - 1);
}
