use super::*;

#[test]
fn spill_io_rejects_truncation_and_drop_removes_private_directory() {
    let root = tempfile::tempdir().unwrap();
    let spill = RuntimeFile::new(root.path()).unwrap();
    let path = spill.path.clone();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            spill
                .file
                .lock()
                .unwrap()
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    spill.append(b"synthetic spill").unwrap();
    assert_eq!(spill.len().unwrap(), 15);
    assert!(spill.read_at(14, &mut [0; 2]).is_err());
    spill.file.lock().unwrap().set_len(0).unwrap();
    assert!(spill.read_at(0, &mut [0; 1]).is_err());
    drop(spill);
    assert!(!path.exists());
}

#[test]
fn process_exit_spill_child() {
    let Some(root) = std::env::var_os("CTX_ATTRIBUTION_SPILL_EXIT_TEST_ROOT") else {
        return;
    };
    let spill = RuntimeFile::new(Path::new(&root)).unwrap();
    spill.append(&vec![0x42; 256 * 1024]).unwrap();
    assert_eq!(spill.len().unwrap(), 256 * 1024);
    // Exercise native handle cleanup, deliberately skipping Rust Drop.
    std::process::exit(0);
}

#[test]
fn process_exit_reclaims_spill_payload_without_rust_drop() {
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "materialization::runtime_file::tests::process_exit_spill_child",
            "--nocapture",
        ])
        .env("CTX_ATTRIBUTION_SPILL_EXIT_TEST_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let directories = std::fs::read_dir(root.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        directories.len(),
        1,
        "child must have created a spill directory"
    );
    assert_eq!(
        std::fs::read_dir(directories[0].path()).unwrap().count(),
        0,
        "native process-exit cleanup must leave no spill payload"
    );
}
