use std::{
    cell::Cell,
    fs,
    io::{Read, Write},
    os::unix::{fs::symlink, fs::PermissionsExt},
    time::{Duration, UNIX_EPOCH},
};

use super::*;
use crate::{ProviderSourceRoot, SourceIoError};

thread_local! {
    static NATIVE_ERROR: Cell<Option<i32>> = const { Cell::new(None) };
    static NATIVE_CALLS: Cell<usize> = const { Cell::new(0) };
    static COMPONENT_OPENS: Cell<usize> = const { Cell::new(0) };
    static OPEN_METADATA: Cell<usize> = const { Cell::new(0) };
    static FILESYSTEM_PROOFS: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn record_component_open() {
    COMPONENT_OPENS.set(COMPONENT_OPENS.get() + 1);
}

pub(super) fn record_open_metadata() {
    OPEN_METADATA.set(OPEN_METADATA.get() + 1);
}

pub(super) fn record_filesystem_proof() {
    FILESYSTEM_PROOFS.set(FILESYSTEM_PROOFS.get() + 1);
}

pub(super) fn before_native_open(_path: &CStr) -> io::Result<()> {
    NATIVE_CALLS.set(NATIVE_CALLS.get() + 1);
    NATIVE_ERROR
        .get()
        .map_or(Ok(()), |errno| Err(io::Error::from_raw_os_error(errno)))
}

struct NativeErrorGuard;

impl NativeErrorGuard {
    fn new(errno: Option<i32>) -> Self {
        assert!(NATIVE_ERROR.replace(errno).is_none());
        NATIVE_CALLS.set(0);
        COMPONENT_OPENS.set(0);
        OPEN_METADATA.set(0);
        FILESYSTEM_PROOFS.set(0);
        Self
    }
}

impl Drop for NativeErrorGuard {
    fn drop(&mut self) {
        NATIVE_ERROR.set(None);
    }
}

fn strategies() -> [Option<i32>; 3] {
    [None, Some(libc::ENOSYS), Some(libc::EPERM)]
}

fn rejection(result: Result<OpenedPath, AuthorityOpenError>) -> &'static str {
    match result {
        Err(AuthorityOpenError::Rejected(reason)) => reason,
        Err(error) => panic!("expected path rejection, got {error:?}"),
        Ok(_) => panic!("unsafe path was admitted"),
    }
}

#[test]
fn absolute_path_validation_precedes_native_and_fallback_io() {
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        for raw in [
            b"".as_slice(),
            b"relative",
            b"./relative",
            b"/../file",
            b"/a/../b",
            b"/a\0/b",
        ] {
            let path = Path::new(OsStr::from_bytes(raw));
            assert!(matches!(
                open_absolute_handle(path),
                Err(AuthorityOpenError::Rejected(_))
            ));
            assert_eq!(NATIVE_CALLS.get(), 0);
        }
    }
}

#[test]
fn native_and_fallback_open_regular_files_directories_and_exact_native_names() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let nested = temp.path().join("目录");
    fs::create_dir(&nested).unwrap();
    let names = [
        OsStr::new("résumé.jsonl"),
        OsStr::from_bytes(b"native-\xff.jsonl"),
    ];
    for name in names {
        fs::write(nested.join(name), b"source bytes\n").unwrap();
    }
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        assert!(matches!(
            open_absolute(&nested).unwrap(),
            OpenedPath::Directory { .. }
        ));
        for name in names {
            let path = nested.join(name);
            let OpenedPath::File {
                mut file, metadata, ..
            } = open_absolute(&path).unwrap()
            else {
                panic!("ordinary source is not a file");
            };
            assert_eq!(metadata.ino(), fs::metadata(&path).unwrap().ino());
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"source bytes\n");
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
}

#[test]
fn long_absolute_paths_preserve_ordinary_sources_and_reject_links_and_long_components() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let short_path = temp.path().join("short.jsonl");
    fs::write(&short_path, b"short source\n").unwrap();
    let mut directory_path = temp.path().to_path_buf();
    let mut directory = File::open(&directory_path).unwrap();
    let component = CString::new("d".repeat(128)).unwrap();
    // Construct the real tree by dirfd: an absolute create_dir_all would itself
    // exceed Linux's single-syscall pathname limit before the test could run.
    while directory_path.as_os_str().as_bytes().len() < 5 * 1024
        || directory_path
            .strip_prefix(temp.path())
            .unwrap()
            .as_os_str()
            .as_bytes()
            .len()
            < libc::PATH_MAX as usize
    {
        // SAFETY: the parent descriptor and NUL-terminated component are live.
        assert_eq!(
            unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700) },
            0,
            "{}",
            io::Error::last_os_error()
        );
        // SAFETY: this opens the just-created directory relative to its parent.
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        assert!(descriptor >= 0, "{}", io::Error::last_os_error());
        // SAFETY: openat returned a fresh owned descriptor.
        directory = unsafe { File::from_raw_fd(descriptor) };
        directory_path.push(OsStr::from_bytes(component.as_bytes()));
    }
    let path = directory_path.join("source.jsonl");
    assert!((5 * 1024..7 * 1024).contains(&path.as_os_str().as_bytes().len()));
    // SAFETY: the directory descriptor and static C string remain valid; mode
    // is supplied because this call creates a fresh fixture file.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c"source.jsonl".as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600 as libc::mode_t,
        )
    };
    assert!(descriptor >= 0, "{}", io::Error::last_os_error());
    // SAFETY: the successful openat transferred ownership of this descriptor.
    let mut original = unsafe { File::from_raw_fd(descriptor) };
    original.write_all(b"long source\n").unwrap();
    let expected_inode = original.metadata().unwrap().ino();
    for (target, name) in [(c"source.jsonl", c"linked-file"), (c".", c"linked-dir")] {
        // SAFETY: both C strings and the directory descriptor remain valid.
        assert_eq!(
            unsafe { libc::symlinkat(target.as_ptr(), directory.as_raw_fd(), name.as_ptr()) },
            0,
            "{}",
            io::Error::last_os_error()
        );
    }
    // Establish the old walk's result independently of native dispatch.
    let mut old = open_absolute_components(&path).unwrap();
    assert_eq!(old.metadata().unwrap().ino(), expected_inode);
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"long source\n");
    let base = ProviderSourceRoot::open(temp.path()).unwrap();
    let relative_path = path.strip_prefix(temp.path()).unwrap();
    assert!(relative_path.as_os_str().as_bytes().len() >= libc::PATH_MAX as usize);

    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        let OpenedPath::Directory { metadata, .. } = open_absolute(&directory_path).unwrap() else {
            panic!("long authority is not a directory");
        };
        assert_eq!(metadata.ino(), directory.metadata().unwrap().ino());
        NATIVE_CALLS.set(0);
        let file = open_absolute_handle(&path).unwrap();
        assert_eq!(
            NATIVE_CALLS.get(),
            0,
            "long paths must select the walk first"
        );
        let OpenedPath::File {
            mut file, metadata, ..
        } = classify_opened(file).unwrap()
        else {
            panic!("long ordinary source is not a file");
        };
        assert_eq!(metadata.ino(), expected_inode);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"long source\n");
        let relative = base.open_file(relative_path).unwrap();
        assert_eq!(relative.metadata().ino(), expected_inode);
        let mut bytes = Vec::new();
        relative.file().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"long source\n");
        for invalid in [
            relative_path.with_file_name("linked-file"),
            relative_path
                .with_file_name("linked-dir")
                .join("source.jsonl"),
            relative_path.with_file_name("x".repeat(256)),
        ] {
            assert!(base.open_path(&invalid).is_err());
        }
        let authority = ProviderSourceRoot::open(&directory_path).unwrap();
        authority
            .open_file(Path::new("source.jsonl"))
            .unwrap()
            .revalidate_leaf_with_same_object_root()
            .unwrap();
        NATIVE_CALLS.set(0);
        for linked in [
            directory_path.join("linked-file"),
            directory_path.join("linked-dir/source.jsonl"),
        ] {
            assert_eq!(
                rejection(open_absolute(&linked)),
                super::super::SYMLINK_PROVIDER_SOURCE_REASON
            );
        }
        assert!(
            matches!(open_absolute(&directory_path.join("x".repeat(256))),
                Err(AuthorityOpenError::Io(error)) if error.raw_os_error() == Some(libc::ENAMETOOLONG))
        );
        assert_eq!(
            NATIVE_CALLS.get(),
            0,
            "long paths must select the walk first"
        );

        // Nearby short paths still exercise native lookup or forced fallback.
        let mut file = open_absolute_handle(&short_path).unwrap();
        assert!(file.metadata().unwrap().is_file());
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"short source\n");
        assert!(matches!(open_absolute(&temp.path().join("x".repeat(256))),
                Err(AuthorityOpenError::Io(error)) if error.raw_os_error() == Some(libc::ENAMETOOLONG)));
        assert_eq!(NATIVE_CALLS.get(), 2);
    }
}

#[path = "unix_relative_tests.rs"]
mod relative_tests;

#[test]
fn absolute_path_normalization_preserves_component_walk_semantics() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    fs::write(&path, b"source\n").unwrap();
    let mut redundant = temp.path().as_os_str().as_bytes().to_vec();
    redundant.extend_from_slice(b"//./source.jsonl/.");
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        let file = open_absolute_handle(Path::new(OsStr::from_bytes(&redundant))).unwrap();
        assert_eq!(
            file.metadata().unwrap().ino(),
            fs::metadata(&path).unwrap().ino()
        );
        assert!(open_absolute_handle(Path::new("////"))
            .unwrap()
            .metadata()
            .unwrap()
            .is_dir());
    }
}

#[test]
fn native_absolute_open_returns_one_nonblocking_cloexec_read_only_handle() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    fs::write(&path, b"source\n").unwrap();
    let _guard = NativeErrorGuard::new(None);
    let file = match open_absolute_native(&validated_absolute_path(&path).unwrap()) {
        Ok(file) => file,
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {
            eprintln!("native openat2 unavailable; forced fallback cases still apply: {error}");
            return;
        }
        Err(error) => panic!("native open failed: {error}"),
    };
    assert_eq!(NATIVE_CALLS.get(), 1);
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(flags & libc::O_ACCMODE, libc::O_RDONLY);
    assert_ne!(flags & libc::O_NONBLOCK, 0);
    assert_ne!(flags & libc::O_NOFOLLOW, 0);
    let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    assert!(descriptor_flags >= 0);
    assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
    assert!(matches!(
        classify_opened(file).unwrap(),
        OpenedPath::File { .. }
    ));
}

#[test]
fn native_path_errors_never_fall_back_to_a_successful_component_walk() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let path = temp.path().join("source.jsonl");
    fs::write(&path, b"source\n").unwrap();
    for errno in [
        libc::EACCES,
        libc::ENOENT,
        libc::ENOTDIR,
        libc::EINVAL,
        libc::E2BIG,
        libc::EAGAIN,
        libc::EXDEV,
        libc::ENAMETOOLONG,
        libc::EMFILE,
        libc::EINTR,
    ] {
        let _guard = NativeErrorGuard::new(Some(errno));
        assert!(
            matches!(open_absolute_handle(&path), Err(AuthorityOpenError::Io(error))
            if error.raw_os_error() == Some(errno))
        );
        assert_eq!(NATIVE_CALLS.get(), 1);
    }
    for (errno, expected) in [
        (libc::ELOOP, super::super::SYMLINK_PROVIDER_SOURCE_REASON),
        (
            libc::ENXIO,
            super::super::NON_REGULAR_PROVIDER_SOURCE_REASON,
        ),
        (
            libc::ENODEV,
            super::super::NON_REGULAR_PROVIDER_SOURCE_REASON,
        ),
        (
            libc::EOPNOTSUPP,
            super::super::NON_REGULAR_PROVIDER_SOURCE_REASON,
        ),
    ] {
        let _guard = NativeErrorGuard::new(Some(errno));
        assert_eq!(rejection(open_absolute(&path)), expected);
        assert_eq!(NATIVE_CALLS.get(), 1);
    }
}

#[test]
fn both_strategies_reject_ancestor_final_and_procfd_symlinks() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let path = nested.join("source.jsonl");
    fs::write(&path, b"source\n").unwrap();
    let file = File::open(&path).unwrap();
    let linked_parent = temp.path().join("linked-parent");
    let linked_file = temp.path().join("linked-file");
    symlink(&nested, &linked_parent).unwrap();
    symlink(&path, &linked_file).unwrap();
    let magic = PathBuf::from(format!(
        "/proc/{}/fd/{}",
        std::process::id(),
        file.as_raw_fd()
    ));
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        for candidate in [
            linked_parent.join("source.jsonl"),
            linked_file.clone(),
            magic.clone(),
        ] {
            assert_eq!(
                rejection(open_absolute(&candidate)),
                super::super::SYMLINK_PROVIDER_SOURCE_REASON
            );
        }
        assert!(open_absolute(&path).is_ok());
    }
}

#[test]
fn both_strategies_reject_special_files_and_preserve_not_a_directory() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let fifo = temp.path().join("fifo.jsonl");
    crate::test_support_paths::make_fifo(&fifo).unwrap();
    let ordinary = temp.path().join("ordinary.jsonl");
    fs::write(&ordinary, b"source\n").unwrap();
    for errno in strategies() {
        let _guard = NativeErrorGuard::new(errno);
        for special in [fifo.as_path(), Path::new("/dev/null")] {
            assert_eq!(
                rejection(open_absolute(special)),
                super::super::NON_REGULAR_PROVIDER_SOURCE_REASON
            );
        }
        assert!(
            matches!(open_absolute(&ordinary.join("child")), Err(AuthorityOpenError::Io(error))
            if error.raw_os_error() == Some(libc::ENOTDIR))
        );
        assert!(matches!(
            open_absolute(&ordinary).unwrap(),
            OpenedPath::File { .. }
        ));
    }
}

#[test]
fn native_and_fallback_retain_filesystem_qualification_and_root_replacement_checks() {
    for errno in strategies() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("root");
        let moved = temp.path().join("moved");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source.jsonl"), b"source\n").unwrap();
        let _guard = NativeErrorGuard::new(errno);
        assert_eq!(
            rejection(open_absolute(Path::new("/proc"))),
            "provider source roots require a qualified local Linux filesystem"
        );
        let authority = ProviderSourceRoot::open(&root).unwrap();
        let source = authority.open_file(Path::new("source.jsonl")).unwrap();
        source.revalidate_leaf_with_same_object_root().unwrap();
        fs::write(root.join("sibling"), b"sibling\n").unwrap();
        source.revalidate_leaf_with_same_object_root().unwrap();
        fs::rename(&root, &moved).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("source.jsonl"), b"source\n").unwrap();
        source.revalidate_leaf().unwrap();
        assert!(source.revalidate_leaf_with_same_object_root().is_err());
        assert!(authority.revalidate_same_object().is_err());
    }
}

#[test]
fn combined_leaf_and_root_fence_remains_exact_for_file_changes() {
    for errno in strategies() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("source.jsonl");
        fs::write(&path, b"before\n").unwrap();
        // Separate the opening stamp from the rewrite without relying on the
        // filesystem clock advancing between two back-to-back writes.
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            )
            .unwrap();
        let before = fs::metadata(&path).unwrap();
        let _guard = NativeErrorGuard::new(errno);
        let authority = ProviderSourceRoot::open(temp.path()).unwrap();
        let source = authority.open_file(Path::new("source.jsonl")).unwrap();
        source.revalidate_leaf_with_same_object_root().unwrap();
        fs::write(&path, b"after!\n").unwrap();
        let after = fs::metadata(&path).unwrap();
        assert_eq!(before.ino(), after.ino());
        assert_eq!(before.len(), after.len());
        assert_ne!(before.modified().unwrap(), after.modified().unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"after!\n");
        assert!(source.revalidate_leaf_with_same_object_root().is_err());
        let fresh = authority.open_file(Path::new("source.jsonl")).unwrap();
        fresh.revalidate_leaf_with_same_object_root().unwrap();
        // The mapped caller must preserve the same typed failure too.
        let mapped = crate::open_provider_source_file_mapped::<SourceIoError>(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(mapped.revalidate_leaf_with_same_object_root().is_err());
    }
}

#[test]
fn native_lookup_requires_search_but_not_read_permission_on_ancestors() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("permission test requires an unprivileged user");
        return;
    }
    let temp = crate::test_support_paths::tempdir().unwrap();
    let ancestor = temp.path().join("search-only");
    fs::create_dir(&ancestor).unwrap();
    let path = ancestor.join("source.jsonl");
    fs::write(&path, b"source\n").unwrap();
    match open_absolute_native(&validated_absolute_path(&path).unwrap()) {
        Ok(_) => {}
        Err(error) if matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {
            eprintln!("permission comparison needs native openat2: {error}");
            return;
        }
        Err(error) => panic!("native open failed: {error}"),
    }
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o100)).unwrap();
    let native = open_absolute(&path);
    let old_kernel = {
        let _guard = NativeErrorGuard::new(Some(libc::ENOSYS));
        open_absolute(&path)
    };
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(native.is_ok());
    assert!(
        matches!(old_kernel, Err(AuthorityOpenError::Io(error)) if error.raw_os_error() == Some(libc::EACCES))
    );

    for restrict_ancestor in [true, false] {
        let restricted = if restrict_ancestor { &ancestor } else { &path };
        fs::set_permissions(restricted, fs::Permissions::from_mode(0o0)).unwrap();
        for errno in strategies() {
            let _guard = NativeErrorGuard::new(errno);
            assert!(
                matches!(open_absolute(&path), Err(AuthorityOpenError::Io(error))
                if error.raw_os_error() == Some(libc::EACCES))
            );
        }
        fs::set_permissions(
            restricted,
            fs::Permissions::from_mode(if restrict_ancestor { 0o700 } else { 0o600 }),
        )
        .unwrap();
    }
}
