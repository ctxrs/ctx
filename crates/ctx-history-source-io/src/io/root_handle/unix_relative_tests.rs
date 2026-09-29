use super::*;

fn native_relative_available(root: &ProviderSourceRoot, path: &Path) -> bool {
    let available = try_open_relative(&root.inner.directory, path, &root.inner.filesystem)
        .unwrap()
        .is_some();
    if !available {
        eprintln!("native relative openat2 unavailable; fallback cases still apply");
    }
    available
}

#[test]
fn relative_validation_and_normalization_preserve_native_names_and_empty_root() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::create_dir(temp.path().join("nested")).unwrap();
    let names = [
        OsStr::new("résumé.jsonl"),
        OsStr::from_bytes(b"native-\xff"),
    ];
    for name in names {
        fs::write(temp.path().join("nested").join(name), b"source bytes\n").unwrap();
    }
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        for raw in [
            b".".as_slice(),
            b"./nested",
            b"../nested",
            b"nested/../file",
            b"/nested",
            b"nested/nu\0l",
        ] {
            assert!(matches!(
                root.open_path(Path::new(OsStr::from_bytes(raw))),
                Err(SourceIoError::InvalidProviderTranscriptPath { .. })
            ));
        }
        let empty = root.open_directory(Path::new("")).unwrap();
        assert_eq!(empty.authority_fingerprint(), root.authority_fingerprint());
        assert_eq!(NATIVE_CALLS.get(), 0);
        assert_eq!(COMPONENT_OPENS.get(), 0);
        for name in names {
            let mut raw = b"nested//./".to_vec();
            raw.extend_from_slice(name.as_bytes());
            raw.extend_from_slice(b"/.");
            let file = root.open_file(Path::new(OsStr::from_bytes(&raw))).unwrap();
            assert_eq!(
                file.metadata().ino(),
                fs::metadata(temp.path().join("nested").join(name))
                    .unwrap()
                    .ino()
            );
            let mut bytes = Vec::new();
            file.file().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"source bytes\n");
        }
    }
}

#[test]
fn relative_reader_reopens_have_independent_cursors() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::create_dir(temp.path().join("nested")).unwrap();
    fs::write(temp.path().join("nested/source"), b"abcdef").unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        let source = root.open_file(Path::new("nested/source")).unwrap();
        let mut first = source.reopen_same_object().unwrap();
        let mut bytes = [0; 2];
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ab");
        let mut second = source.reopen_same_object().unwrap();
        second.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"ab");
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"cd");
    }
}

#[test]
fn relative_open_work_is_constant_with_depth_and_fallback_observes_each_component_once() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    if root.inner.filesystem.filesystem_type == ecryptfs::SUPER_MAGIC {
        eprintln!("work bound excludes eCryptfs's separate backing-path qualification");
        return;
    }
    for depth in [1, 4, 12] {
        let mut relative = PathBuf::new();
        for _ in 1..depth {
            relative.push("nested");
        }
        fs::create_dir_all(temp.path().join(&relative)).unwrap();
        relative.push("source");
        fs::write(temp.path().join(&relative), b"source\n").unwrap();
        let native = native_relative_available(&root, &relative);
        for errno in strategies() {
            let _guard = NativeErrorGuard::new(errno);
            let source = root.open_file(&relative).unwrap();
            assert_eq!(source.len(), 7);
            let mut bytes = Vec::new();
            source.file().read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"source\n");
            assert_eq!(NATIVE_CALLS.get(), 1);
            if errno.is_none() && native {
                assert_eq!(COMPONENT_OPENS.get(), 0);
                assert_eq!(OPEN_METADATA.get(), 1);
                assert_eq!(FILESYSTEM_PROOFS.get(), 1);
            } else {
                assert_eq!(COMPONENT_OPENS.get(), depth);
                assert_eq!(OPEN_METADATA.get(), depth);
                assert_eq!(FILESYSTEM_PROOFS.get(), depth);
            }
        }
    }
}

#[test]
fn ordinary_child_open_observes_metadata_once_and_still_classifies_types() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::write(temp.path().join("file"), b"source\n").unwrap();
    fs::create_dir(temp.path().join("directory")).unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    if root.inner.filesystem.filesystem_type == ecryptfs::SUPER_MAGIC {
        eprintln!("work bound excludes eCryptfs's separate backing-path qualification");
        return;
    }
    let directory = root.directory().unwrap();
    for (name, is_file) in [("file", true), ("directory", false)] {
        let _guard = NativeErrorGuard::new(None);
        let child = directory.open_child(OsStr::new(name)).unwrap();
        assert_eq!(
            matches!(child, crate::OpenedProviderSourcePath::File(_)),
            is_file
        );
        assert_eq!(NATIVE_CALLS.get(), 0);
        assert_eq!(COMPONENT_OPENS.get(), 1);
        assert_eq!(OPEN_METADATA.get(), 1);
        assert_eq!(FILESYSTEM_PROOFS.get(), 1);
    }
}

#[test]
fn relative_native_errors_never_reach_a_successful_component_walk() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::write(temp.path().join("source"), b"source\n").unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    for errno in [
        libc::EACCES,
        libc::ENOENT,
        libc::EINVAL,
        libc::E2BIG,
        libc::EAGAIN,
        libc::ENAMETOOLONG,
        libc::EMFILE,
        libc::EINTR,
    ] {
        let _guard = NativeErrorGuard::new(Some(errno));
        assert!(
            matches!(root.open_file(Path::new("source")), Err(SourceIoError::Io(error))
            if error.raw_os_error() == Some(errno))
        );
        assert_eq!(NATIVE_CALLS.get(), 1);
        assert_eq!(COMPONENT_OPENS.get(), 0);
        assert_eq!(OPEN_METADATA.get(), 0);
    }
    for (errno, expected) in [
        (libc::EXDEV, "same filesystem mount"),
        (libc::ENOTDIR, "ancestor components must be directories"),
        (libc::ELOOP, "symlinked provider source path components"),
        (libc::ENXIO, "regular files or directories"),
        (libc::ENODEV, "regular files or directories"),
        (libc::EOPNOTSUPP, "regular files or directories"),
    ] {
        let _guard = NativeErrorGuard::new(Some(errno));
        assert!(matches!(root.open_file(Path::new("source")),
            Err(SourceIoError::InvalidProviderTranscriptPath { reason, .. })
            if reason.contains(expected)));
        assert_eq!(NATIVE_CALLS.get(), 1);
        assert_eq!(COMPONENT_OPENS.get(), 0);
    }
    assert_eq!(root.open_file(Path::new("source")).unwrap().len(), 7);
}

#[test]
fn relative_native_and_fallback_reject_links_special_files_and_non_directory_ancestors() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::create_dir(temp.path().join("nested")).unwrap();
    fs::write(temp.path().join("nested/source"), b"source\n").unwrap();
    fs::hard_link(
        temp.path().join("nested/source"),
        temp.path().join("hardlink"),
    )
    .unwrap();
    symlink("nested/source", temp.path().join("linked-file")).unwrap();
    symlink("nested", temp.path().join("linked-directory")).unwrap();
    crate::test_support_paths::make_fifo(&temp.path().join("fifo")).unwrap();
    // Construct through a short dirfd alias so a long test-output directory
    // does not exceed the Unix socket address limit.
    let fixture_directory = File::open(temp.path()).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(format!(
        "/proc/self/fd/{}/socket",
        fixture_directory.as_raw_fd()
    ))
    .unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        for path in ["linked-file", "linked-directory/source"] {
            assert!(matches!(root.open_path(Path::new(path)),
                Err(SourceIoError::InvalidProviderTranscriptPath { reason, .. })
                if reason.contains("symlinked")));
        }
        for path in ["fifo", "socket", "fifo/child", "nested/source/child"] {
            assert!(matches!(
                root.open_path(Path::new(path)),
                Err(SourceIoError::InvalidProviderTranscriptPath { .. })
            ));
        }
        let ordinary = root.open_file(Path::new("nested/source")).unwrap();
        let hardlink = root.open_file(Path::new("hardlink")).unwrap();
        assert_eq!(ordinary.metadata().ino(), hardlink.metadata().ino());
        ordinary.revalidate_leaf_with_same_object_root().unwrap();
    }
}

#[test]
fn ancestor_replacement_keeps_leaf_and_directory_proofs_distinct() {
    for errno in strategies() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("root");
        fs::create_dir_all(path.join("nested")).unwrap();
        fs::create_dir(path.join("alternate")).unwrap();
        fs::write(path.join("nested/source"), b"original\n").unwrap();
        fs::hard_link(path.join("nested/source"), path.join("alternate/source")).unwrap();
        let root = ProviderSourceRoot::open(&path).unwrap();
        let _guard = NativeErrorGuard::new(errno);
        let directory = root.open_directory(Path::new("nested")).unwrap();
        let source = root.open_file(Path::new("nested/source")).unwrap();
        fs::rename(path.join("nested"), path.join("saved")).unwrap();
        fs::rename(path.join("alternate"), path.join("nested")).unwrap();
        // The same leaf remains reachable; no new intermediate-directory
        // identity contract is imposed on a leaf-only observation.
        source.revalidate_leaf_with_same_object_root().unwrap();
        assert!(directory.revalidate_same_object().is_err());
        fs::write(path.join("replacement"), b"different\n").unwrap();
        fs::rename(path.join("replacement"), path.join("nested/source")).unwrap();
        assert!(source.revalidate_leaf_with_same_object_root().is_err());
        let mut bytes = Vec::new();
        source.file().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original\n");
        fs::rename(&path, temp.path().join("moved")).unwrap();
        fs::create_dir(&path).unwrap();
        let retained = root.open_file(Path::new("nested/source")).unwrap();
        let mut bytes = Vec::new();
        retained.file().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"different\n");
        assert!(root.revalidate_same_object().is_err());
    }
}

#[test]
fn relative_permission_checks_allow_search_only_ancestors_but_deny_missing_access() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("permission test requires an unprivileged user");
        return;
    }
    let temp = crate::test_support_paths::tempdir().unwrap();
    let directory = temp.path().join("nested");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("source");
    fs::write(&path, b"source\n").unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    let relative = Path::new("nested/source");
    if !native_relative_available(&root, relative) {
        return;
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o100)).unwrap();
    let native = root.open_file(relative);
    let fallback = {
        let _guard = NativeErrorGuard::new(Some(libc::ENOSYS));
        root.open_file(relative)
    };
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(native.is_ok());
    assert!(
        matches!(fallback, Err(SourceIoError::Io(error)) if error.raw_os_error() == Some(libc::EACCES))
    );
    for restrict_directory in [true, false] {
        let restricted = if restrict_directory {
            &directory
        } else {
            &path
        };
        fs::set_permissions(restricted, fs::Permissions::from_mode(0o0)).unwrap();
        for errno in strategies() {
            let _guard = NativeErrorGuard::new(errno);
            assert!(
                matches!(root.open_file(relative), Err(SourceIoError::Io(error))
                if error.raw_os_error() == Some(libc::EACCES))
            );
        }
        fs::set_permissions(
            restricted,
            fs::Permissions::from_mode(if restrict_directory { 0o700 } else { 0o600 }),
        )
        .unwrap();
    }
}

#[test]
fn relative_native_retains_terminal_mount_identity_comparison() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    fs::write(temp.path().join("source"), b"source\n").unwrap();
    let root = ProviderSourceRoot::open(temp.path()).unwrap();
    let relative = Path::new("source");
    if !native_relative_available(&root, relative) {
        return;
    }
    let mut wrong_mount = root.inner.filesystem.clone();
    wrong_mount.mount_id ^= 1;
    assert!(matches!(
        try_open_relative(&root.inner.directory, relative, &wrong_mount),
        Err(AuthorityOpenError::Rejected(
            "provider source descendants may not cross filesystem mounts"
        ))
    ));
    assert!(
        try_open_relative(&root.inner.directory, relative, &root.inner.filesystem)
            .unwrap()
            .is_some()
    );
}

#[test]
fn relative_native_rejects_a_real_mount_before_classifying_the_destination() {
    let Ok(root) = ProviderSourceRoot::open(Path::new("/")) else {
        eprintln!("real mount check requires a qualified filesystem at /");
        return;
    };
    // /proc is a real mount boundary. Without NO_XDEV, the kernel can open it;
    // destination qualification alone would reject it later, which is too late
    // to prove that an intermediate mount was never traversed.
    match open_native(
        root.inner.directory.as_raw_fd(),
        c"proc",
        libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
    ) {
        Ok(file) => {
            let metadata = file.metadata().unwrap();
            assert!(metadata.is_dir());
            if metadata.dev() == root.inner.directory.metadata().unwrap().dev() {
                eprintln!("/proc is not a different filesystem in this environment");
                return;
            }
        }
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOSYS | libc::EPERM | libc::ENOENT)
            ) =>
        {
            eprintln!("real mount/native comparison unavailable: {error}");
            return;
        }
        Err(error) => panic!("unrestricted native lookup failed: {error}"),
    }
    {
        let _guard = NativeErrorGuard::new(None);
        assert!(matches!(root.open_path(Path::new("proc")),
            Err(SourceIoError::InvalidProviderTranscriptPath { reason, .. })
            if reason.contains("same filesystem mount")));
        assert_eq!(NATIVE_CALLS.get(), 1);
        assert_eq!(COMPONENT_OPENS.get(), 0);
        assert_eq!(FILESYSTEM_PROOFS.get(), 0);
    }
    for errno in [libc::ENOSYS, libc::EPERM] {
        let _guard = NativeErrorGuard::new(Some(errno));
        assert!(matches!(
            root.open_path(Path::new("proc")),
            Err(SourceIoError::InvalidProviderTranscriptPath { .. })
        ));
        assert_eq!(COMPONENT_OPENS.get(), 1);
    }
    assert_eq!(
        root.open_directory(Path::new(""))
            .unwrap()
            .authority_fingerprint(),
        root.authority_fingerprint()
    );
}
