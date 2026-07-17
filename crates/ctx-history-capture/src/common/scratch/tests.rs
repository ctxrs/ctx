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

    fn scratch_run_entry_count(root: &Path) -> usize {
        fs::read_dir(root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("run-") || name.starts_with(".ctx-cleanup-run-")
            })
            .count()
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
        assert_eq!(scratch_run_entry_count(&root), 9);
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
    fn scratch_cleanup_handoff_does_not_delete_a_swapped_live_run() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        let replacement = create_abandoned_run(&root, 1);
        let sentinel = replacement.join("sentinel");
        fs::write(&sentinel, b"must survive").unwrap();
        let replacement_lease = open_private_regular_file(&replacement.join(LEASE_NAME)).unwrap();
        FileExt::lock_exclusive(&replacement_lease).unwrap();
        let run = UnixScratchRun::open(&target).unwrap();
        let target_lease = run.open_lease().unwrap().unwrap();
        FileExt::lock_exclusive(&target_lease).unwrap();
        let moved_target = root.join("handoff-original");
        fs::rename(&target, &moved_target).unwrap();
        fs::rename(&replacement, &target).unwrap();

        let mut budget = ScratchCleanupBudget::new();
        let error = remove_anchored_scratch_run(
            &run,
            &target,
            UnixScratchRunLocation::Canonical,
            &mut budget,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("changed identity"));
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"must survive");
        assert!(moved_target.join(OWNER_NAME).exists());
        FileExt::unlock(&target_lease).unwrap();
        FileExt::unlock(&replacement_lease).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unix_scratch_pre_unlink_failures_restore_the_canonical_name() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let points = [
            #[cfg(not(target_os = "freebsd"))]
            UnixScratchFinalizationFailurePoint::AfterRename,
            UnixScratchFinalizationFailurePoint::AfterFirstIdentityCheck,
            UnixScratchFinalizationFailurePoint::AfterSecondIdentityCheck,
            UnixScratchFinalizationFailurePoint::BeforeUnlink,
        ];

        for (index, point) in points.into_iter().enumerate() {
            let target = create_abandoned_run(&root, index as u64);
            let quarantine = scratch_quarantine_path(&target).unwrap();
            let run = UnixScratchRun::open(&target).unwrap();
            let target_lease = run.open_lease().unwrap().unwrap();
            FileExt::lock_exclusive(&target_lease).unwrap();
            inject_unix_scratch_finalization_failure_once(point, false);

            let mut budget = ScratchCleanupBudget::new();
            let error = remove_anchored_scratch_run(
                &run,
                &target,
                UnixScratchRunLocation::Canonical,
                &mut budget,
            )
            .unwrap_err();

            assert!(error.to_string().contains("injected"));
            run.revalidate(&target).unwrap();
            assert!(!quarantine.exists());
            FileExt::unlock(&target_lease).unwrap();
        }
    }

    #[cfg(target_os = "freebsd")]
    #[test]
    fn unix_scratch_failure_after_unlink_does_not_restore_a_removed_run() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        let quarantine = scratch_quarantine_path(&target).unwrap();
        let run = UnixScratchRun::open(&target).unwrap();
        let target_lease = run.open_lease().unwrap().unwrap();
        FileExt::lock_exclusive(&target_lease).unwrap();
        inject_unix_scratch_finalization_failure_once(
            UnixScratchFinalizationFailurePoint::AfterUnlink,
            false,
        );

        let mut budget = ScratchCleanupBudget::new();
        let error = remove_anchored_scratch_run(
            &run,
            &target,
            UnixScratchRunLocation::Canonical,
            &mut budget,
        )
        .unwrap_err();

        assert!(error.to_string().contains("AfterUnlink"));
        assert!(!target.exists());
        assert!(!quarantine.exists());
        assert_eq!(run.directory_link_count().unwrap(), 0);
        FileExt::unlock(&target_lease).unwrap();
    }

    #[cfg(all(unix, not(target_os = "freebsd")))]
    #[test]
    fn unix_scratch_restore_collision_is_reclaimed_after_the_collision_clears() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        let quarantine = scratch_quarantine_path(&target).unwrap();
        let run = UnixScratchRun::open(&target).unwrap();
        let target_lease = run.open_lease().unwrap().unwrap();
        FileExt::lock_exclusive(&target_lease).unwrap();
        inject_unix_scratch_finalization_failure_once(
            UnixScratchFinalizationFailurePoint::AfterRename,
            true,
        );

        let mut budget = ScratchCleanupBudget::new();
        let error = remove_anchored_scratch_run(
            &run,
            &target,
            UnixScratchRunLocation::Canonical,
            &mut budget,
        )
        .unwrap_err();

        assert!(error.to_string().contains("quarantine retained"));
        assert!(target.is_dir());
        assert!(quarantine.is_dir());
        FileExt::unlock(&target_lease).unwrap();
        drop(target_lease);
        fs::remove_dir(&target).unwrap();

        let _manager_lock = acquire_manager_lock(&root).unwrap();
        let mut cleanup_budget = ScratchCleanupBudget::new();
        assert_eq!(
            cleanup_abandoned_scratch_run(&target, &mut cleanup_budget).unwrap(),
            ScratchCleanupOutcome::Complete
        );
        assert!(!quarantine.exists());
        assert_eq!(scratch_run_entry_count(&root), 0);
    }

    #[cfg(all(unix, not(target_os = "freebsd")))]
    #[test]
    fn unix_scratch_atomic_quarantine_rename_never_overwrites_a_destination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        let quarantine = scratch_quarantine_path(&target).unwrap();
        create_private_directory(&quarantine).unwrap();
        fs::write(quarantine.join("sentinel"), b"must survive").unwrap();
        let run = UnixScratchRun::open(&target).unwrap();
        let target_lease = run.open_lease().unwrap().unwrap();
        FileExt::lock_exclusive(&target_lease).unwrap();

        let mut budget = ScratchCleanupBudget::new();
        let error = remove_anchored_scratch_run(
            &run,
            &target,
            UnixScratchRunLocation::Canonical,
            &mut budget,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        run.revalidate(&target).unwrap();
        assert_eq!(
            fs::read(quarantine.join("sentinel")).unwrap(),
            b"must survive"
        );
        FileExt::unlock(&target_lease).unwrap();
    }

    #[cfg(all(unix, not(target_os = "freebsd")))]
    #[test]
    fn unix_private_root_contract_removes_the_verified_empty_quarantine() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        let quarantine = scratch_quarantine_path(&target).unwrap();
        let run = UnixScratchRun::open(&target).unwrap();
        let target_lease = run.open_lease().unwrap().unwrap();
        FileExt::lock_exclusive(&target_lease).unwrap();
        let mut budget = ScratchCleanupBudget::new();

        // The checked name-based final rmdir relies on this same-user 0700 root. Symlinks,
        // non-owned entries, and identity changes before rmdir remain rejected.
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o077, 0);
        assert_eq!(
            remove_anchored_scratch_run(
                &run,
                &target,
                UnixScratchRunLocation::Canonical,
                &mut budget,
            )
            .unwrap(),
            ScratchCleanupOutcome::Complete
        );

        assert!(!target.exists());
        assert!(!quarantine.exists());
        assert_eq!(run.directory_link_count().unwrap(), 0);
        FileExt::unlock(&target_lease).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn oversized_file_cleanup_resumes_at_the_same_cursor_and_converges() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = run_path(&root, 0);
        let quarantine = scratch_quarantine_path(&target).unwrap();
        create_private_directory(&target).unwrap();
        let payload_path = target.join("oversized.bin");
        let payload = create_private_file(&payload_path).unwrap();
        let truncate_step = 1024_u64;
        let initial_bytes = truncate_step * 3;
        payload.set_len(initial_bytes).unwrap();
        drop(payload);
        let max_bytes = SCRATCH_DELETE_ROW_OVERHEAD_BYTES + truncate_step;

        let first_budget = ScratchCleanupBudget::with_max_bytes_for_test(max_bytes);
        scavenge_abandoned_runs_with_budget(&root, 1, first_budget).unwrap();

        assert_eq!(
            fs::metadata(&payload_path).unwrap().len(),
            initial_bytes - truncate_step
        );
        let mut state_file = open_or_create_private_control_file(&root, SWEEP_STATE_NAME).unwrap();
        assert_eq!(read_sweep_state(&mut state_file, 2).unwrap().cursor, 0);
        assert_eq!(scratch_run_entry_count(&root), 1);
        drop(state_file);

        let mut converged = false;
        for _ in 0..4 {
            let budget = ScratchCleanupBudget::with_max_bytes_for_test(max_bytes);
            scavenge_abandoned_runs_with_budget(&root, 1, budget).unwrap();
            let mut state_file =
                open_or_create_private_control_file(&root, SWEEP_STATE_NAME).unwrap();
            if read_sweep_state(&mut state_file, 2).unwrap().cursor == 2 {
                converged = true;
                break;
            }
        }

        assert!(converged);
        assert!(!target.exists());
        assert!(!quarantine.exists());
        assert_eq!(scratch_run_entry_count(&root), 0);
    }

    #[cfg(unix)]
    #[test]
    fn huge_scratch_cleanup_returns_pending_within_one_internal_budget() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        for index in 0..(SCRATCH_DELETE_FILES_PER_PAGE * (SCRATCH_CLEANUP_MAX_PAGES + 2)) {
            fs::write(target.join(format!("payload-{index:04}")), b"bounded").unwrap();
        }
        let run = UnixScratchRun::open(&target).unwrap();
        let lease = run.open_lease().unwrap().unwrap();
        FileExt::lock_exclusive(&lease).unwrap();
        let mut budget = ScratchCleanupBudget::new();

        let outcome = remove_anchored_scratch_run(
            &run,
            &target,
            UnixScratchRunLocation::Canonical,
            &mut budget,
        )
        .unwrap();

        assert_eq!(outcome, ScratchCleanupOutcome::Pending);
        assert!(budget.pages <= SCRATCH_CLEANUP_MAX_PAGES);
        assert!(budget.operations <= SCRATCH_CLEANUP_MAX_OPERATIONS);
        assert!(budget.bytes <= SCRATCH_CLEANUP_MAX_BYTES);
        assert!(fs::read_dir(&target).unwrap().next().is_some());
        FileExt::unlock(&lease).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn startup_scavenging_retains_the_cursor_for_a_pending_huge_run() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        for index in 0..(SCRATCH_DELETE_FILES_PER_PAGE * (SCRATCH_CLEANUP_MAX_PAGES + 2)) {
            fs::write(target.join(format!("payload-{index:04}")), b"bounded").unwrap();
        }
        seed_next_run_id(&root, 1);

        let current = CaptureScratchSpace::create_in(root.clone(), "bounded-revisit").unwrap();
        let mut state_file = open_or_create_private_control_file(&root, SWEEP_STATE_NAME).unwrap();
        let state = read_sweep_state(&mut state_file, 2).unwrap();

        assert_eq!(state.cursor, 0);
        assert!(target.exists());
        drop(current);
    }

    #[cfg(windows)]
    #[test]
    fn windows_owned_drop_removes_the_scratch_run() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let scratch = CaptureScratchSpace::create_in(root, "owned-drop").unwrap();
        let path = scratch.path().to_path_buf();

        drop(scratch);

        assert!(!path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_abandoned_scavenger_removes_the_scratch_run() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let path = create_abandoned_run(&root, 0);

        let mut budget = ScratchCleanupBudget::new();
        assert_eq!(
            cleanup_abandoned_scratch_run(&path, &mut budget).unwrap(),
            ScratchCleanupOutcome::Complete
        );
        assert!(!path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_live_lease_is_not_removed_by_scavenging() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let path = create_abandoned_run(&root, 0);
        let lease = open_private_regular_file(&path.join(LEASE_NAME)).unwrap();
        FileExt::lock_exclusive(&lease).unwrap();

        let mut budget = ScratchCleanupBudget::new();
        assert_eq!(
            cleanup_abandoned_scratch_run(&path, &mut budget).unwrap(),
            ScratchCleanupOutcome::Busy
        );
        assert!(path.exists());
        FileExt::unlock(&lease).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_scratch_child_open_denies_directory_aba() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        ensure_private_directory(&root).unwrap();
        let target = create_abandoned_run(&root, 0);
        let replacement = create_abandoned_run(&root, 1);
        fs::write(replacement.join("sentinel"), b"must survive").unwrap();
        let replacement_lease = open_private_regular_file(&replacement.join(LEASE_NAME)).unwrap();
        FileExt::lock_exclusive(&replacement_lease).unwrap();
        let run = WindowsScratchRun::open(&target).unwrap();
        let target_lease = run.open_lease(&target).unwrap().unwrap();
        FileExt::lock_exclusive(&target_lease).unwrap();
        let moved_target = root.join("handoff-original");
        inject_windows_scratch_child_aba_once(moved_target.clone(), replacement.clone());

        let mut budget = ScratchCleanupBudget::new();
        let error = remove_anchored_scratch_run(&run, &target, Some(target_lease), &mut budget)
            .unwrap_err();

        assert!(error.raw_os_error().is_some());
        assert!(target.join(OWNER_NAME).exists());
        assert_eq!(
            fs::read(replacement.join("sentinel")).unwrap(),
            b"must survive"
        );
        assert!(!moved_target.exists());
        FileExt::unlock(&replacement_lease).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_finalization_closes_delete_pending_lease_before_directory_delete() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch");
        let mut scratch = CaptureScratchSpace::create_in(root, "delete-pending").unwrap();
        let path = scratch.path().to_path_buf();
        let lease = scratch.lease.take().unwrap();

        let mut budget = ScratchCleanupBudget::new();
        assert_eq!(
            cleanup_owned_scratch_run(&path, lease, &mut budget).unwrap(),
            ScratchCleanupOutcome::Complete
        );

        assert!(!path.exists());
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
