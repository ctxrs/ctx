mod tests {
    use super::*;

    fn seed_next_run_id(root: &Path, next: u64) {
        let mut file = open_or_create_private_control_file(root, NEXT_RUN_ID_NAME).unwrap();
        write_control_file(&mut file, &format!("{next}\n")).unwrap();
    }

    fn create_abandoned_run(root: &Path, run_id: u64) -> PathBuf {
        let run = run_path(root, run_id);
        create_private_directory(&run).unwrap();
        drop(create_private_file(&run.join(LEASE_NAME)).unwrap());
        drop(create_private_file(&run.join(OWNER_NAME)).unwrap());
        run
    }

    #[test]
    fn live_scratch_lease_is_not_scavenged() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let first = CaptureScratchSpace::create_in(root.clone(), "first").unwrap();
        let first_path = first.path().to_path_buf();
        let second = CaptureScratchSpace::create_in(root, "second").unwrap();

        assert!(first_path.exists());
        assert!(second.path().exists());
    }

    #[test]
    fn startup_scavenging_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        for index in 0..(MAX_SCAVENGE_RUNS + 8) {
            create_abandoned_run(&root, index as u64);
        }
        seed_next_run_id(&root, (MAX_SCAVENGE_RUNS + 8) as u64);

        let current = CaptureScratchSpace::create_in(root.clone(), "bounded").unwrap();
        let runs = fs::read_dir(root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("run-"))
            .count();
        assert_eq!(runs, 9);
        assert!(current.path().exists());
    }

    #[test]
    fn scratch_teardown_is_size_aware_and_operation_paced() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let scratch = CaptureScratchSpace::create_in(root, "paced-delete").unwrap();
        let scratch_path = scratch.path().to_path_buf();
        let payload_bytes = 256 * 1024usize;
        let mut payload = scratch.create_file("payload.bin").unwrap();
        payload.write_all(&vec![b'x'; payload_bytes]).unwrap();
        payload.sync_all().unwrap();
        drop(payload);
        let pacer = crate::DiskIoPacer::new(u64::MAX, u64::MAX);
        let _pacing = crate::install_disk_io_pacer(pacer.clone());
        let bytes_before = pacer.charged_bytes();
        let operations_before = pacer.filesystem_operation_count();

        drop(scratch);

        assert!(!scratch_path.exists());
        assert!(pacer.charged_bytes() >= bytes_before.saturating_add(payload_bytes as u64));
        assert!(pacer.filesystem_operation_count() > operations_before);
    }

    #[cfg(unix)]
    #[test]
    fn scratch_cleanup_rejects_a_renamed_run_replaced_by_a_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let scratch = CaptureScratchSpace::create_in(root, "tamper").unwrap();
        let scratch_path = scratch.path().to_path_buf();
        for index in 0..(SCRATCH_DELETE_FILES_PER_PAGE + 8) {
            fs::write(
                scratch_path.join(format!("payload-{index:03}.bin")),
                b"private scratch data",
            )
            .unwrap();
        }
        let moved_path = temp.path().join("moved-run");
        let replacement_target = temp.path().join("replacement-target");
        fs::create_dir(&replacement_target).unwrap();
        let sentinel = replacement_target.join("sentinel");
        fs::write(&sentinel, b"must survive").unwrap();
        inject_scratch_cleanup_tamper_once(1, moved_path.clone(), replacement_target.clone());

        let error = remove_private_scratch_run(&scratch_path).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("changed identity"));
        assert!(fs::read_dir(&moved_path).unwrap().next().is_some());
        assert!(scratch_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read(sentinel).unwrap(), b"must survive");
        fs::remove_file(&scratch_path).unwrap();
        fs::rename(&moved_path, &scratch_path).unwrap();
        drop(scratch);
        assert!(!scratch_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn scavenging_fails_closed_on_a_linked_run_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        symlink(temp.path(), run_path(&root, 0)).unwrap();
        seed_next_run_id(&root, 1);

        let error = CaptureScratchSpace::create_in(root, "unsafe")
            .err()
            .unwrap();
        assert!(error.to_string().contains("private directory"));
    }

    #[test]
    fn scavenging_advances_across_gaps_and_wraps_to_revisit_live_runs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let live_path = run_path(&root, 0);
        create_private_directory(&live_path).unwrap();
        let live_lease = create_private_file(&live_path.join(LEASE_NAME)).unwrap();
        drop(create_private_file(&live_path.join(OWNER_NAME)).unwrap());
        FileExt::lock_exclusive(&live_lease).unwrap();
        create_abandoned_run(&root, 31);
        seed_next_run_id(&root, 40);

        let first = CaptureScratchSpace::create_in(root.clone(), "first-pass").unwrap();
        assert!(live_path.exists());
        assert!(!run_path(&root, 31).exists());
        drop(first);

        FileExt::unlock(&live_lease).unwrap();
        drop(live_lease);
        let second = CaptureScratchSpace::create_in(root.clone(), "finish-window").unwrap();
        assert!(live_path.exists());
        drop(second);
        let third = CaptureScratchSpace::create_in(root, "wrapped-pass").unwrap();
        assert!(!live_path.exists());
        assert!(third.path().exists());
    }

    #[test]
    fn corrupt_sweep_state_fails_closed_without_deleting_runs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let abandoned = create_abandoned_run(&root, 0);
        seed_next_run_id(&root, 1);
        let mut state = open_or_create_private_control_file(&root, SWEEP_STATE_NAME).unwrap();
        write_control_file(&mut state, "not-a-sweep-state\n").unwrap();

        let error = CaptureScratchSpace::create_in(root, "corrupt")
            .err()
            .unwrap();
        assert!(error.to_string().contains("sweep state is corrupt"));
        assert!(abandoned.exists());
    }

    #[test]
    fn concurrent_creators_receive_unique_monotonic_run_ids() {
        use std::sync::{Arc, Barrier, Mutex};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let barrier = Arc::new(Barrier::new(8));
        let names = Arc::new(Mutex::new(Vec::new()));
        std::thread::scope(|scope| {
            for index in 0..8 {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                let names = Arc::clone(&names);
                scope.spawn(move || {
                    let scratch = CaptureScratchSpace::create_in(root, "concurrent").unwrap();
                    names.lock().unwrap().push(
                        scratch
                            .path()
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                    let _ = index;
                    barrier.wait();
                });
            }
        });
        let mut names = names.lock().unwrap().clone();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 8);
        assert_eq!(names.first().unwrap(), "run-00000000000000000000");
        assert_eq!(names.last().unwrap(), "run-00000000000000000007");
    }

    #[test]
    fn exhausted_run_id_space_fails_without_reusing_an_owner_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        seed_next_run_id(&root, u64::MAX);

        let error = CaptureScratchSpace::create_in(root, "exhausted")
            .err()
            .unwrap();
        assert!(error.to_string().contains("run ID space is exhausted"));
    }

    #[cfg(windows)]
    #[test]
    fn existing_scratch_root_with_permissive_dacl_is_rejected() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::{LocalFree, ERROR_SUCCESS},
            Security::{
                Authorization::{
                    ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW,
                    SE_FILE_OBJECT,
                },
                GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION,
                PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
            },
        };

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        fs::create_dir(&root).unwrap();
        let sddl = "D:P(A;OICI;FA;;;WD)"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            },
            0
        );
        assert_ne!(present, 0);
        let wide = root
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        assert_eq!(
            unsafe {
                SetNamedSecurityInfoW(
                    wide.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null_mut(),
                )
            },
            ERROR_SUCCESS
        );
        unsafe {
            LocalFree(descriptor);
        }

        let error = CaptureScratchSpace::create_in(root, "permissive")
            .err()
            .unwrap();
        assert!(error.to_string().contains("DACL is not owner/System-only"));
    }

    #[cfg(unix)]
    #[test]
    fn scratch_directories_and_files_are_private() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let scratch =
            CaptureScratchSpace::create_in(temp.path().join("scratch"), "privacy").unwrap();
        let file = scratch.create_file("private.sqlite").unwrap();
        let root_mode = fs::metadata(scratch.path().parent().unwrap())
            .unwrap()
            .mode();
        let run_mode = fs::metadata(scratch.path()).unwrap().mode();
        let file_mode = file.metadata().unwrap().mode();

        assert_eq!(root_mode & 0o077, 0);
        assert_eq!(run_mode & 0o077, 0);
        assert_eq!(file_mode & 0o077, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn scratch_hard_crash_helper() {
        let Some(root) = std::env::var_os("CTX_CAPTURE_SCRATCH_CRASH_ROOT") else {
            return;
        };
        let marker = PathBuf::from(
            std::env::var_os("CTX_CAPTURE_SCRATCH_CRASH_MARKER").expect("marker path"),
        );
        let scratch = CaptureScratchSpace::create_in(PathBuf::from(root), "crash-helper").unwrap();
        fs::write(&marker, scratch.path().as_os_str().as_encoded_bytes()).unwrap();
        unsafe {
            libc::kill(libc::getpid(), libc::SIGKILL);
        }
        unreachable!();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn next_run_scavenges_scratch_abandoned_by_sigkill() {
        use std::os::unix::ffi::OsStringExt;
        use std::process::Command;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let marker = temp.path().join("crashed-run");
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("common::scratch::tests::scratch_hard_crash_helper")
            .arg("--nocapture")
            .env("CTX_CAPTURE_SCRATCH_CRASH_ROOT", &root)
            .env("CTX_CAPTURE_SCRATCH_CRASH_MARKER", &marker)
            .status()
            .unwrap();
        assert!(!status.success());

        let crashed = PathBuf::from(std::ffi::OsString::from_vec(fs::read(&marker).unwrap()));
        assert!(crashed.exists());
        let next = CaptureScratchSpace::create_in(root, "next-run").unwrap();
        assert!(!crashed.exists());
        assert!(next.path().exists());
    }
}
